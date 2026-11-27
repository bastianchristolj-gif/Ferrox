# Ferrox

Navigateur web en Rust avec une interface « gaming » sombre. Il est pensé pour être léger et pour protéger la vie privée.
Le binaire release fait environ **1,2 Mo**.

## Fonctionnalités

| | |
|---|---|
| **Control** | Jauges RAM et CPU en direct, mesurées sur le navigateur et tous ses processus. Limiteur de RAM, nombre maximum d'onglets actifs, mise en veille automatique des onglets inactifs, bouton « Endormir » par onglet. |
| **Bouclier** | Bloqueur de publicités, de traqueurs et de cryptomineurs : blocage des navigations, de `fetch`, XHR, `sendBeacon`, des balises script/img/iframe et masquage CSS des emplacements pub. Un compteur affiche le nombre d'éléments bloqués par page. |
| **Sécurité** | Mode HTTPS uniquement. Les permissions caméra, micro et position sont refusées. `file://` et `javascript:` sont refusés. Les popups s'ouvrent dans des onglets. Global Privacy Control et DNT sont envoyés. Option anti-empreinte (sans WebGL ni WebAudio). Onglets privés. |
| **Linux** | Les processus web sont isolés dans un **sandbox bubblewrap** si `bwrap` est installé. ITP (anti-pistage intersites) est activé. Le préchargement DNS et le cache de pages sont désactivés. |
| **Interface** | Page *Paramètres* complète (`ferrox://settings`) avec recherche, 6 thèmes, couleur d'accent libre, animations désactivables. Choix du moteur de recherche (DuckDuckGo par défaut), page d'accueil personnalisée. Fermer le dernier onglet quitte le navigateur. |

### Raccourcis

`Ctrl+T` nouvel onglet · `Ctrl+Maj+N` onglet privé · `Ctrl+W` fermer · `Ctrl+L` barre d'adresse · `Ctrl+R` recharger · `Ctrl+Tab` onglet suivant · clic molette pour fermer un onglet.

## Pourquoi c'est léger

Ferrox n'embarque **pas** Chromium. Il utilise le moteur web déjà présent sur le système :

- **Linux** : WebKitGTK
- **Windows** : WebView2 (Edge)
- **macOS** : WKWebView (Safari)

Le code de Ferrox lui-même (interface, onglets, sécurité, limiteurs) est écrit en Rust : pas de garbage collector, et la mémoire est sûre.

## Windows (cible principale)

**Utiliser** : décompresser `Ferrox-windows-x64.zip` et double-cliquer sur `Ferrox.exe`. Garder `WebView2Loader.dll` dans le même dossier.
Prérequis : Windows 10 ou 11 64 bits. Le moteur WebView2 (celui d'Edge) est déjà inclus dans Windows.
Comme l'exe n'est pas signé, SmartScreen peut afficher « Windows a protégé votre ordinateur ». Cliquez alors sur *Informations complémentaires*, puis *Exécuter quand même*.

Ce qui est propre à Windows :
- fenêtre sans bordure avec ombre native et icône ;
- SmartScreen anti-phishing actif (wry le coupe par défaut, Ferrox le réactive) ;
- télémétrie et pings coupés ;
- sandbox Chromium de WebView2 ;
- onglets privés en mode InPrivate ;
- **extensions Chrome** : sur la page *Extensions* (icône puzzle ou `ferrox://extensions`), collez le lien d'une extension du Chrome Web Store puis cliquez sur *Installer*. Vous pouvez ensuite l'activer ou la désactiver, ouvrir sa fenêtre ou ses options, ou la supprimer. WebView2 ne prend en charge qu'une partie des API Chrome : toutes les extensions ne fonctionnent pas.

**Compiler sur Windows** : installer [Rust](https://rustup.rs) (avec les *Visual Studio Build Tools*), puis :
```powershell
cargo build --release
```
L'exe est produit dans `target\release\ferrox.exe`.

**Compiler depuis Linux** (compilation croisée) :
```bash
sudo apt install mingw-w64 && rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

**GitHub Actions** : `.github/workflows/windows.yml` compile et teste sur un vrai Windows à chaque push. L'exe est téléchargeable dans l'onglet *Actions*.

## Compiler (Linux / macOS)

Prérequis : [Rust](https://rustup.rs).

**Linux (Debian/Ubuntu)** :
```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev bubblewrap xdg-dbus-proxy
```
**Fedora** : `sudo dnf install webkit2gtk4.1-devel gtk3-devel bubblewrap`
**Arch** : `sudo pacman -S webkit2gtk-4.1 bubblewrap`

**Windows / macOS** : rien de plus à installer (WebView2 est inclus dans Windows 10/11).

```bash
cargo build --release
./target/release/ferrox            # ou : ferrox https://exemple.fr
```

## Configuration

- Paramètres : `%APPDATA%\ferrox\settings.json` sous Windows, `~/.config/ferrox/settings.json` sous Linux (modifiables depuis l'interface)
- Liste de blocage personnalisée : `blocklist.txt` dans le même dossier. Un domaine par ligne, le format « hosts » est accepté. Vous pouvez par exemple y coller la liste [StevenBlack/hosts](https://github.com/StevenBlack/hosts).
- Profil (cookies, cache) : `%LOCALAPPDATA%\ferrox\profile` sous Windows, `~/.local/share/ferrox/profile` sous Linux

## Limites connues (v0.1)

- Le bloqueur agit au niveau des navigations et du JavaScript de la page. Il n'intercepte pas encore les requêtes au niveau réseau : une ressource déclarée en dur dans le HTML peut partir avant d'être retirée.
- Le mode HTTPS uniquement **bloque** les liens `http://` et propose « Autoriser ce site ». Il ne réessaie pas automatiquement en HTTPS.
- Pas encore de favoris, d'historique ni de restauration de session.
- Extensions : Windows uniquement. Le bouton d'action d'une extension s'ouvre dans un onglet, pas dans une bulle.
- La RAM affichée est la RSS de chaque processus. Les bibliothèques partagées sont donc un peu surcomptées.

## Structure

```
src/main.rs     fenêtre, onglets, IPC, sécurité, limiteurs
src/blocker.rs  bloqueur pubs/traqueurs
src/monitor.rs  mesure RAM/CPU
src/config.rs   paramètres persistants (validés)
ui/index.html   interface (onglets, barre, panneaux)
ui/home.html    page Nouvel onglet
```
