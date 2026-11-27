//! Extensions web (format Chrome / Manifest V3).
//!
//! - **Windows** : WebView2 (moteur d'Edge) charge les extensions dans le profil.
//!   Installation depuis le Chrome Web Store : téléchargement du `.crx`,
//!   vérification que la clé publique correspond bien à l'ID demandé,
//!   extraction sécurisée (pas de chemins `..`), puis ajout au profil.
//! - **Linux / macOS** : WebKit ne prend pas en charge les extensions Chrome.

use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_CRX: u64 = 64 * 1024 * 1024;
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;
const MAX_FILES: usize = 10_000;

pub const SUPPORTED: bool = cfg!(target_os = "windows");

/// uBlock Origin Lite (Manifest V3, par l'auteur d'uBlock Origin).
/// uBlock Origin « classique » (Manifest V2) n'est plus accepté par le moteur d'Edge / WebView2.
pub const UBOL_ID: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";

pub fn ext_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ferrox")
        .join("extensions")
}

/// Extrait l'ID (32 lettres a–p) d'une URL du Chrome Web Store ou d'un ID brut.
pub fn parse_store_id(input: &str) -> Option<String> {
    input
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|s| s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b)))
        .map(str::to_string)
}

/// ID Chrome = 16 premiers octets du SHA-256 de la clé publique, en lettres a–p.
pub fn id_from_pubkey(key: &[u8]) -> String {
    Sha256::digest(key)[..16]
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .map(|n| (b'a' + n) as char)
        .collect()
}

// --------------------------------------------------------------------- CRX3

fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Champs « length-delimited » d'un message protobuf : (numéro, contenu).
fn proto_fields(buf: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let Some(tag) = varint(buf, &mut pos) else { break };
        match tag & 7 {
            0 => {
                if varint(buf, &mut pos).is_none() {
                    break;
                }
            }
            1 => pos += 8,
            5 => pos += 4,
            2 => {
                let Some(len) = varint(buf, &mut pos) else { break };
                let end = pos.saturating_add(len as usize);
                if end > buf.len() {
                    break;
                }
                out.push((tag >> 3, &buf[pos..end]));
                pos = end;
            }
            _ => break,
        }
    }
    out
}

/// Découpe un fichier CRX3 : renvoie l'archive ZIP et la clé publique dont
/// l'ID correspond à `expected_id` (si trouvée).
pub fn parse_crx3<'a>(data: &'a [u8], expected_id: &str) -> Result<(&'a [u8], Option<Vec<u8>>), String> {
    if data.len() < 12 || &data[..4] != b"Cr24" {
        return Err("fichier CRX invalide".into());
    }
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if version != 3 {
        return Err(format!("version CRX {version} non prise en charge"));
    }
    let hlen = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let header = data.get(12..12 + hlen).ok_or("en-tête CRX tronqué")?;
    let zip = &data[12 + hlen..];
    // CrxFileHeader : 2 = sha256_with_rsa, 3 = sha256_with_ecdsa (AsymmetricKeyProof)
    let key = proto_fields(header)
        .into_iter()
        .filter(|(n, _)| *n == 2 || *n == 3)
        .flat_map(|(_, proof)| proto_fields(proof))
        .filter(|(n, _)| *n == 1)
        .map(|(_, k)| k.to_vec())
        .find(|k| id_from_pubkey(k) == expected_id);
    Ok((zip, key))
}

// -------------------------------------------------------- Téléchargement

fn http_agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(90)))
        .max_redirects(8)
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36 Edg/131.0")
        .build()
        .into()
}

/// Télécharge l'extension `id` depuis le Chrome Web Store et l'extrait dans
/// `ext_dir()/<id>`. Bloquant : à appeler depuis un thread secondaire.
pub fn download_and_unpack(id: &str) -> Result<PathBuf, String> {
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=131.0&acceptformat=crx3&x=id%3D{id}%26installsource%3Dondemand%26uc"
    );
    let mut resp = http_agent().get(&url).call().map_err(|e| format!("téléchargement impossible : {e}"))?;
    let data = resp
        .body_mut()
        .with_config()
        .limit(MAX_CRX)
        .read_to_vec()
        .map_err(|e| format!("lecture impossible : {e}"))?;
    if data.is_empty() {
        return Err("extension introuvable sur le Chrome Web Store".into());
    }
    let (zip_bytes, key) = parse_crx3(&data, id)?;
    let key = key.ok_or("signature de l'extension ne correspondant pas à l'ID demandé")?;
    unpack(zip_bytes, id, Some(&key))
}

fn unpack(zip_bytes: &[u8], id: &str, key: Option<&[u8]>) -> Result<PathBuf, String> {
    let root = ext_dir();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let tmp = root.join(format!(".tmp-{id}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).map_err(|e| format!("archive invalide : {e}"))?;
    if zip.len() > MAX_FILES {
        return Err("archive trop volumineuse".into());
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name() refuse les chemins absolus et les « .. » (zip-slip).
        let Some(rel) = f.enclosed_name() else { return Err("chemin dangereux dans l'archive".into()) };
        let dest = tmp.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut buf = Vec::new();
        (&mut f).take(MAX_UNPACKED - total + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        total += buf.len() as u64;
        if total > MAX_UNPACKED {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err("extension trop volumineuse".into());
        }
        std::fs::write(&dest, buf).map_err(|e| e.to_string())?;
    }
    // Ajoute la clé publique au manifeste : l'extension garde son ID officiel.
    let mpath = tmp.join("manifest.json");
    let text = std::fs::read_to_string(&mpath).map_err(|_| "manifest.json manquant".to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    let mut manifest: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("manifest.json invalide : {e}"))?;
    if let (Some(obj), Some(k)) = (manifest.as_object_mut(), key) {
        obj.insert("key".into(), base64::engine::general_purpose::STANDARD.encode(k).into());
        obj.remove("update_url");
    }
    std::fs::write(&mpath, serde_json::to_vec_pretty(&manifest).unwrap_or_default()).map_err(|e| e.to_string())?;

    let dest = root.join(id);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

// ---------------------------------------------------------- Métadonnées

#[derive(Serialize, Clone, Debug, Default)]
pub struct ExtInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub version: String,
    pub description: String,
    pub popup: Option<String>,
    pub options: Option<String>,
    pub icon: Option<String>,
}

fn localize(dir: &Path, manifest: &serde_json::Value, s: &str) -> String {
    let Some(key) = s.strip_prefix("__MSG_").and_then(|k| k.strip_suffix("__")) else { return s.to_string() };
    let default = manifest["default_locale"].as_str().unwrap_or("en");
    for loc in ["fr", default, "en"] {
        let p = dir.join("_locales").join(loc).join("messages.json");
        if let Ok(t) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.trim_start_matches('\u{feff}')) {
                if let Some(obj) = v.as_object() {
                    if let Some((_, m)) = obj.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                        if let Some(msg) = m["message"].as_str() {
                            return msg.to_string();
                        }
                    }
                }
            }
        }
    }
    s.to_string()
}

fn safe_rel(p: &str) -> Option<String> {
    let p = p.trim_start_matches('/');
    (!p.is_empty() && !p.contains("..") && !p.contains(':')).then(|| p.to_string())
}

/// Lit le manifeste d'une extension installée par Ferrox.
pub fn info_from_disk(id: &str) -> Option<ExtInfo> {
    let dir = ext_dir().join(id);
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let m: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let s = |k: &str| m[k].as_str().map(|v| localize(&dir, &m, v)).unwrap_or_default();
    let popup = m["action"]["default_popup"]
        .as_str()
        .or(m["browser_action"]["default_popup"].as_str())
        .and_then(safe_rel);
    let options = m["options_ui"]["page"].as_str().or(m["options_page"].as_str()).and_then(safe_rel);
    let icon = m["icons"].as_object().and_then(|icons| {
        let mut v: Vec<(u32, &str)> =
            icons.iter().filter_map(|(k, p)| Some((k.parse().ok()?, p.as_str()?))).collect();
        v.sort();
        let (_, path) = v.iter().rev().find(|(s, _)| *s <= 128).or(v.first())?;
        let path = safe_rel(path)?;
        let bytes = std::fs::read(dir.join(&path)).ok().filter(|b| b.len() < 256 * 1024)?;
        let mime = if path.ends_with(".svg") { "image/svg+xml" } else { "image/png" };
        Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
    });
    Some(ExtInfo {
        id: id.to_string(),
        name: s("name"),
        enabled: true,
        version: s("version"),
        description: s("description").chars().take(200).collect(),
        popup,
        options,
        icon,
    })
}

// -------------------------------------------------------- WebView2 (Windows)

#[cfg(target_os = "windows")]
pub mod wv2 {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{
        BrowserExtensionEnableCompletedHandler, BrowserExtensionRemoveCompletedHandler,
        ProfileAddBrowserExtensionCompletedHandler, ProfileGetBrowserExtensionsCompletedHandler, take_pwstr,
    };
    use windows_core::{BOOL, HSTRING, Interface, PWSTR};
    use wry::WebViewExtWindows;

    fn profile(wv: &wry::WebView) -> windows_core::Result<ICoreWebView2Profile7> {
        unsafe { wv.webview().cast::<ICoreWebView2_13>()?.Profile()?.cast::<ICoreWebView2Profile7>() }
    }

    /// Saisie automatique et enregistrement des mots de passe (réglages du profil).
    pub fn set_autofill(wv: &wry::WebView, autofill: bool, passwords: bool) {
        unsafe {
            if let Ok(p) = wv.webview().cast::<ICoreWebView2_13>().and_then(|w| w.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile6>()) {
                let _ = p.SetIsGeneralAutofillEnabled(autofill);
                let _ = p.SetIsPasswordAutosaveEnabled(passwords);
            }
        }
    }

    /// Vidéo en plein écran : prévient quand la page entre / sort du plein écran.
    pub fn on_fullscreen(wv: &wry::WebView, cb: impl Fn(bool) + 'static) {
        let handler = webview2_com::ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
            if let Some(s) = sender {
                let mut b = BOOL::default();
                unsafe {
                    let _ = s.ContainsFullScreenElement(&mut b);
                }
                cb(b.as_bool());
            }
            Ok(())
        }));
        let mut token = 0i64;
        unsafe {
            let _ = wv.webview().add_ContainsFullScreenElementChanged(&handler, &mut token);
        }
    }

    /// Icône officielle du site (fournie par le moteur).
    pub fn on_favicon(wv: &wry::WebView, cb: impl Fn(String) + 'static) {
        let Ok(w15) = wv.webview().cast::<ICoreWebView2_15>() else { return };
        let handler = webview2_com::FaviconChangedEventHandler::create(Box::new(move |sender, _| {
            if let Some(w) = sender.and_then(|s| s.cast::<ICoreWebView2_15>().ok()) {
                let mut p = PWSTR::null();
                unsafe {
                    if w.FaviconUri(&mut p).is_ok() {
                        let u = take_pwstr(p);
                        if !u.is_empty() {
                            cb(u);
                        }
                    }
                }
            }
            Ok(())
        }));
        let mut token = 0i64;
        unsafe {
            let _ = w15.add_FaviconChanged(&handler, &mut token);
        }
    }

    /// Identifiant du cadre principal d'une vue (pour relier un onglet à son processus).
    pub fn main_frame_id(wv: &wry::WebView) -> Option<u32> {
        let w = wv.webview().cast::<ICoreWebView2_20>().ok()?;
        let mut id = 0u32;
        unsafe { w.FrameId(&mut id).ok()? };
        Some(id)
    }

    /// Processus de rendu et cadres principaux qu'ils affichent : (pid, [frame ids]).
    pub fn renderer_frames(wv: &wry::WebView, cb: impl FnOnce(Vec<(u32, Vec<u32>)>) + 'static) {
        let Ok(env) = wv.environment().cast::<ICoreWebView2Environment13>() else { return };
        let handler = webview2_com::GetProcessExtendedInfosCompletedHandler::create(Box::new(move |hr, list| {
            let mut out = Vec::new();
            if let (Ok(()), Some(list)) = (hr, list) {
                unsafe {
                    let mut n = 0u32;
                    let _ = list.Count(&mut n);
                    for i in 0..n {
                        let Ok(info) = list.GetValueAtIndex(i) else { continue };
                        let Ok(pi) = info.ProcessInfo() else { continue };
                        let mut pid = 0i32;
                        let _ = pi.ProcessId(&mut pid);
                        let mut frames = Vec::new();
                        if let Ok(coll) = info.AssociatedFrameInfos() {
                            if let Ok(it) = coll.GetIterator() {
                                let mut has = BOOL::default();
                                let _ = it.HasCurrent(&mut has);
                                while has.as_bool() {
                                    if let Ok(f) = it.GetCurrent().and_then(|f| f.cast::<ICoreWebView2FrameInfo2>()) {
                                        // cadre principal = sans parent
                                        if f.ParentFrameInfo().is_err() {
                                            let mut fid = 0u32;
                                            if f.FrameId(&mut fid).is_ok() {
                                                frames.push(fid);
                                            }
                                        }
                                    }
                                    if it.MoveNext(&mut has).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        if !frames.is_empty() {
                            out.push((pid as u32, frames));
                        }
                    }
                }
            }
            cb(out);
            Ok(())
        }));
        unsafe {
            let _ = env.GetProcessExtendedInfos(&handler);
        }
    }

    fn ext_id(e: &ICoreWebView2BrowserExtension) -> String {
        let mut p = PWSTR::null();
        unsafe { if e.Id(&mut p).is_ok() { take_pwstr(p) } else { String::new() } }
    }

    fn ext_name(e: &ICoreWebView2BrowserExtension) -> String {
        let mut p = PWSTR::null();
        unsafe { if e.Name(&mut p).is_ok() { take_pwstr(p) } else { String::new() } }
    }

    fn ext_enabled(e: &ICoreWebView2BrowserExtension) -> bool {
        let mut b = BOOL::default();
        unsafe { e.IsEnabled(&mut b).is_ok() && b.as_bool() }
    }

    fn with_list(
        wv: &wry::WebView,
        f: impl FnOnce(Vec<ICoreWebView2BrowserExtension>) + 'static,
    ) -> Result<(), String> {
        let prof = profile(wv).map_err(|e| format!("extensions indisponibles (WebView2 trop ancien ?) : {e}"))?;
        let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |hr, list| {
            let mut out = Vec::new();
            if hr.is_ok() {
                if let Some(list) = list {
                    let mut n = 0u32;
                    unsafe {
                        let _ = list.Count(&mut n);
                        for i in 0..n {
                            if let Ok(e) = list.GetValueAtIndex(i) {
                                out.push(e);
                            }
                        }
                    }
                }
            }
            f(out);
            Ok(())
        }));
        unsafe { prof.GetBrowserExtensions(&handler) }.map_err(|e| e.to_string())
    }

    /// Liste (id, nom, activée) de toutes les extensions du profil.
    pub fn list(wv: &wry::WebView, cb: impl FnOnce(Vec<(String, String, bool)>) + 'static) -> Result<(), String> {
        with_list(wv, move |exts| cb(exts.iter().map(|e| (ext_id(e), ext_name(e), ext_enabled(e))).collect()))
    }

    pub fn add(wv: &wry::WebView, path: &std::path::Path, cb: impl FnOnce(Result<String, String>) + 'static) -> Result<(), String> {
        let prof = profile(wv).map_err(|e| e.to_string())?;
        let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |hr, ext| {
            cb(match (hr, ext) {
                (Ok(()), Some(e)) => Ok(ext_name(&e)),
                (Err(e), _) => Err(format!("WebView2 a refusé l'extension : {e}")),
                _ => Err("WebView2 a refusé l'extension".into()),
            });
            Ok(())
        }));
        let p = HSTRING::from(path);
        unsafe { prof.AddBrowserExtension(&p, &handler) }.map_err(|e| e.to_string())
    }

    pub fn remove(wv: &wry::WebView, id: String, cb: impl FnOnce(bool) + 'static) -> Result<(), String> {
        with_list(wv, move |exts| match exts.into_iter().find(|e| ext_id(e) == id) {
            Some(e) => {
                let h = BrowserExtensionRemoveCompletedHandler::create(Box::new(move |hr| {
                    cb(hr.is_ok());
                    Ok(())
                }));
                unsafe {
                    let _ = e.Remove(&h);
                }
            }
            None => cb(true),
        })
    }

    pub fn enable(wv: &wry::WebView, id: String, on: bool, cb: impl FnOnce(bool) + 'static) -> Result<(), String> {
        with_list(wv, move |exts| {
            if let Some(e) = exts.into_iter().find(|e| ext_id(e) == id) {
                let h = BrowserExtensionEnableCompletedHandler::create(Box::new(move |hr| {
                    cb(hr.is_ok());
                    Ok(())
                }));
                unsafe {
                    let _ = e.Enable(on, &h);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_ids() {
        let id = "ddkjiahejlhfcafbddmgiahcphecmpfh";
        assert_eq!(parse_store_id(id).as_deref(), Some(id));
        assert_eq!(
            parse_store_id(&format!("https://chromewebstore.google.com/detail/ublock-origin-lite/{id}?hl=fr")).as_deref(),
            Some(id)
        );
        assert_eq!(parse_store_id("https://exemple.fr/zzz"), None);
    }

    /// Téléchargement réel (réseau) : `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_store_download() {
        let id = "ddkjiahejlhfcafbddmgiahcphecmpfh"; // uBlock Origin Lite
        let dir = download_and_unpack(id).expect("téléchargement");
        let m: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let key = base64::engine::general_purpose::STANDARD.decode(m["key"].as_str().unwrap()).unwrap();
        assert_eq!(id_from_pubkey(&key), id);
        let info = info_from_disk(id).unwrap();
        assert!(!info.name.is_empty() && !info.name.starts_with("__MSG_"));
        println!("{} {} popup={:?} options={:?} icon={}", info.name, info.version, info.popup, info.options, info.icon.is_some());
    }

    #[test]
    fn crx_roundtrip() {
        // CRX3 synthétique : en-tête protobuf avec une clé, suivi d'un « zip ».
        let key = b"fake-public-key-bytes".to_vec();
        let id = id_from_pubkey(&key);
        let mut proof = vec![0x0a, key.len() as u8];
        proof.extend(&key);
        let mut header = vec![0x12, proof.len() as u8];
        header.extend(&proof);
        let mut crx = b"Cr24".to_vec();
        crx.extend(3u32.to_le_bytes());
        crx.extend((header.len() as u32).to_le_bytes());
        crx.extend(&header);
        crx.extend(b"PK-zip");
        let (zip, k) = parse_crx3(&crx, &id).unwrap();
        assert_eq!(zip, b"PK-zip");
        assert_eq!(k.as_deref(), Some(key.as_slice()));
        assert!(parse_crx3(&crx, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap().1.is_none());
        assert!(parse_crx3(b"nope", &id).is_err());
    }
}
