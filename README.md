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

`Ctrl+T` nouvel onglet · `Ctrl+Maj+N` onglet privé · `Ctrl+W` fermer · `Ctrl+Maj+T` rouvrir l'onglet fermé · `Ctrl+Tab` / `Ctrl+Maj+Tab` onglet suivant / précédent · `Ctrl+1…9` aller à l'onglet n · `Ctrl+L` barre d'adresse · `Ctrl+R` / `F5` recharger · `Alt+←/→` précédent / suivant · `Ctrl +/-/0` zoom · `F11` plein écran · `Échap` quitter le plein écran ou fermer un menu · clic molette pour fermer un onglet.

### Paramètres (`ferrox://settings`)

Apparence (6 thèmes, couleur d'accent, animations, RAM, fond animé) · moteur de recherche · page d'accueil et **restauration de session** · confidentialité (bloqueur, HTTPS uniquement, permissions, anti-empreinte, Ne pas me pister, effacement à la fermeture) · performances (limiteur de RAM, veille) · onglets (position, fermeture au dernier onglet, zoom par défaut) · saisie automatique et mots de passe · lecture automatique des vidéos · dossier de téléchargement · accélération matérielle · redémarrage · réinitialisation.

### Historique (`ferrox://history`, Ctrl+H)

Les visites sont regroupées par jour et l'historique se recherche. Tu peux supprimer une visite, plusieurs visites cochées, toutes les visites d'un site, ou une période (dernière heure, 24 h, 7 jours, 4 semaines, tout). L'enregistrement peut être mis en pause. Les onglets privés ne sont jamais enregistrés. L'historique est stocké localement dans `history.jsonl`.

### Control (vraies mesures)

Le panneau affiche :
- la RAM et le CPU de Ferrox, et sa part dans la mémoire de l'ordinateur ;
- la répartition par rôle : pages web, moteur, carte graphique, réseau, extensions ;
- la RAM de chaque onglet (sous Windows, via l'API WebView2) ;
- les compteurs d'onglets en mémoire et en veille.

### uBlock Origin

Au premier lancement sous Windows, Ferrox installe automatiquement **uBlock Origin Lite** depuis le Chrome Web Store. uBlock Origin « classique » (Manifest V2) n'est plus accepté par le moteur d'Edge (WebView2). La version Lite (Manifest V3) est faite par le même auteur. D'autres extensions recommandées s'installent en un clic depuis la page *Extensions*.

## Pourquoi c'est léger

Ferrox n'embarque **pas** Chromium. Il utilise le moteur web déjà présent sur le système :

- **Linux** : WebKitGTK
- **Windows** : WebView2 (Edge)
- **macOS** : WKWebView (Safari)

Le code de Ferrox lui-même (interface, onglets, sécurité, limiteurs) est écrit en Rust : pas de garbage collector, et la mémoire est sûre.

## Windows (cible principale)

**Installer** : lancer `Ferrox-Setup-<version>.exe`. L'installation se fait dans votre profil (`%LOCALAPPDATA%\Programs\Ferrox`), sans droits administrateur. Elle crée un raccourci dans le menu Démarrer (et sur le bureau en option) et ajoute Ferrox aux « Applications installées » pour la désinstallation. Le désinstallateur demande s'il faut aussi supprimer vos données. Pour reconstruire l'installateur depuis Linux : `installer/build.sh` (NSIS).

**Version portable** : décompresser `Ferrox-windows-x64.zip` et double-cliquer sur `Ferrox.exe`. Garder `WebView2Loader.dll` dans le même dossier.
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
