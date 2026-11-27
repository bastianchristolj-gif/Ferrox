# Ferrox

Navigateur web pour Windows 10/11, écrit en Rust, avec une interface « gaming » sombre. Il est pensé pour être léger et pour protéger la vie privée.
Depuis la version 2, Ferrox embarque son propre moteur **Chromium** (via [CEF](https://bitbucket.org/chromiumembedded/cef), avec [cef-rs](https://github.com/tauri-apps/cef-rs)) : comme Opera GX, tous les processus du navigateur sont des processus **Ferrox**, regroupés sous Ferrox dans le Gestionnaire des tâches.

## Fonctionnalités

| | |
|---|---|
| **Control** | Jauges RAM et CPU en direct, mesurées sur le navigateur et tous ses processus. Limiteur de RAM, nombre maximum d'onglets actifs, mise en veille automatique des onglets inactifs, bouton « Endormir » par onglet. |
| **Bouclier** | Bloqueur de publicités, de traqueurs et de cryptomineurs. Le moteur annule les requêtes réseau vers les domaines bloqués avant qu'elles ne partent. Ferrox bloque aussi `fetch`, XHR, `sendBeacon` et les balises script/img/iframe, et masque les emplacements pub en CSS. Un compteur affiche le nombre d'éléments bloqués par page. |
| **Sécurité** | Bac à sable de Chromium pour chaque processus de page. Mode HTTPS uniquement. Les permissions caméra, micro et position sont refusées par défaut ; si vous les autorisez, Ferrox vous demande pour chaque site. `file://`, `javascript:` et `chrome://` sont refusés. Les popups s'ouvrent dans des onglets. Global Privacy Control et DNT sont envoyés. Option anti-empreinte (sans WebGL). Onglets privés (en mémoire uniquement). |
| **Interface** | Page *Paramètres* complète (`ferrox://settings`) avec recherche, 6 thèmes, couleur d'accent libre, animations désactivables. Moteur de recherche : Brave Search (par défaut) ou Google, page d'accueil personnalisée. Fermer le dernier onglet quitte le navigateur. |

### Raccourcis

`Ctrl+T` nouvel onglet · `Ctrl+Maj+N` onglet privé · `Ctrl+W` fermer · `Ctrl+Maj+T` rouvrir l'onglet fermé · `Ctrl+Tab` / `Ctrl+Maj+Tab` onglet suivant / précédent · `Ctrl+1…9` aller à l'onglet n · `Ctrl+L` barre d'adresse · `Ctrl+R` / `F5` recharger · `Alt+←/→` précédent / suivant · `Ctrl +/-/0` zoom · `F11` plein écran · `Échap` quitter le plein écran ou fermer un menu · clic molette pour fermer un onglet.

### Paramètres (`ferrox://settings`)

Apparence (6 thèmes, couleur d'accent, animations, RAM, fond animé) · moteur de recherche · page d'accueil et **restauration de session** · confidentialité (bloqueur, HTTPS uniquement, permissions, anti-empreinte, Ne pas me pister, effacement à la fermeture) · performances (limiteur de RAM, veille, mode économie) · onglets (position, fermeture au dernier onglet, zoom par défaut) · lecture automatique des vidéos · dossier de téléchargement · accélération matérielle · redémarrage · réinitialisation.

### Historique (`ferrox://history`, Ctrl+H)

Les visites sont regroupées par jour et l'historique se recherche. Tu peux supprimer une visite, plusieurs visites cochées, toutes les visites d'un site, ou une période (dernière heure, 24 h, 7 jours, 4 semaines, tout). L'enregistrement peut être mis en pause. Les onglets privés ne sont jamais enregistrés. L'historique est stocké localement dans `history.jsonl`.

### Control (vraies mesures)

Le panneau affiche :
- la RAM et le CPU de Ferrox, et sa part dans la mémoire de l'ordinateur ;
- la répartition par rôle : pages web, carte graphique, réseau, services ;
- la RAM de chaque onglet (le processus de page de chaque onglet est connu) ;
- les compteurs d'onglets en mémoire et en veille.

## Installation

**Installer** : lancer `Ferrox-Setup-<version>.exe`. L'installation se fait dans votre profil (`%LOCALAPPDATA%\Programs\Ferrox`), sans droits administrateur. Elle crée un raccourci dans le menu Démarrer (et sur le bureau en option) et ajoute Ferrox aux « Applications installées » pour la désinstallation. Le désinstallateur demande s'il faut aussi supprimer vos données.

**Version portable** : décompresser `Ferrox-<version>-windows.zip` et double-cliquer sur `Ferrox-portable\Ferrox.exe`. Gardez tous les fichiers du dossier ensemble : c'est le moteur Chromium de Ferrox.

Prérequis : Windows 10 ou 11, 64 bits. Rien d'autre à installer.

## Compiler

La compilation se fait sous Windows (MSVC). Le moteur CEF est téléchargé automatiquement par `cef-dll-sys` au premier build.

Prérequis : Rust (stable, cible `x86_64-pc-windows-msvc`), Visual Studio Build Tools (C++), CMake et Ninja.

```powershell
cargo build --release
```

`target\release` contient alors `ferrox.dll` et les fichiers du moteur. `Ferrox.exe` est le lanceur `bootstrap.exe` de CEF renommé : il charge `ferrox.dll` et active le bac à sable de Chromium. La CI GitHub (`.github/workflows/windows.yml`) assemble le dossier complet, l'installateur NSIS et le zip.

Vérification rapide sans les binaires CEF, depuis n'importe quel système :

```bash
rustup target add x86_64-pc-windows-msvc
cargo check --target x86_64-pc-windows-msvc --no-default-features --features dox
```

## Configuration

- Paramètres : `%APPDATA%\ferrox\settings.json` (modifiables depuis l'interface)
- Liste de blocage personnalisée : `blocklist.txt` dans le même dossier. Un domaine par ligne, le format « hosts » est accepté. Vous pouvez par exemple y coller la liste [StevenBlack/hosts](https://github.com/StevenBlack/hosts).
- Profil du navigateur (cookies, cache) : `%LOCALAPPDATA%\ferrox\profile\chromium`

## Sécurité

- **Moteur** : Chromium embarqué (CEF), avec le bac à sable de Chromium. Chaque site tourne dans un processus de page isolé. Aucun rapport de plantage ni ping n'est envoyé.
- **Canal privé page → Ferrox** : dans le processus de page, le script de Ferrox s'exécute avant ceux du site et reçoit une fonction native d'envoi **en argument** : elle n'est jamais placée dans `window`, le site ne peut pas l'atteindre. Chaque onglet a en plus une clé secrète de 128 bits. Les faux événements clavier (`isTrusted = false`) sont ignorés.
- **Interface** : elle a sa propre clé, remise après son chargement. Ses commandes ne sont acceptées que depuis son propre document.
- **Page d'accueil** : ses raccourcis ne sont modifiables que depuis la page d'accueil de Ferrox elle-même.
- **Navigation** : `file://`, `javascript:` et `chrome://` sont refusés, le mode HTTPS uniquement est actif, et caméra, micro et position sont refusés par défaut.
- **Aucun service tiers** : les icônes des sites sont chargées depuis les sites eux-mêmes.
- **Intégrité** : `SHA256SUMS.txt` accompagne chaque version. Pour vérifier un fichier : `certutil -hashfile Ferrox-Setup.exe SHA256`.

## Limites connues

- **Mises à jour de sécurité** : le moteur Chromium est livré avec Ferrox. Il n'est plus mis à jour par Windows : il faut installer les nouvelles versions de Ferrox.
- **Extensions** : CEF ne prend pas en charge les extensions du Chrome Web Store. Le bloqueur intégré de Ferrox remplace uBlock Origin Lite.
- **SmartScreen** (anti-phishing de Microsoft) n'est pas disponible dans Chromium embarqué.
- Pas de gestionnaire de mots de passe intégré pour l'instant.
- Les fichiers ne sont pas signés numériquement (certificat payant) : Windows affiche un avertissement au premier lancement.
- Le mode HTTPS uniquement **bloque** les liens `http://` et propose « Autoriser ce site ». Il ne réessaie pas automatiquement en HTTPS.

## Structure

```
src/lib.rs      fenêtre, onglets, IPC, sécurité, limiteurs (point d'entrée RunWinMain)
src/engine.rs   moteur Chromium (CEF) : vues, canal natif, blocage réseau, téléchargements
src/blocker.rs  bloqueur pubs/traqueurs
src/monitor.rs  mesure RAM/CPU
src/config.rs   paramètres persistants (validés)
src/history.rs  historique
src/icons.rs    icônes des sites
ui/index.html   interface (onglets, barre, panneaux)
ui/home.html    page Nouvel onglet
ui/menu.js      menu clic droit
```
