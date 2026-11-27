## Ferrox V1

Première version publique de Ferrox : un navigateur web léger et sécurisé pour Windows 10/11, écrit en Rust, au style « gaming ».

### Installation
- **`Ferrox-Setup-1.0.0.exe`** : installateur. Aucun droit administrateur n'est demandé. Il crée un raccourci dans le menu Démarrer et, en option, sur le bureau, et se désinstalle depuis « Applications installées ».
- **`Ferrox-1.0.0-windows.zip`** : contient l'installateur, une version portable (sans installation) et les empreintes SHA-256.

> Les fichiers ne sont pas signés numériquement. Au premier lancement, Windows peut afficher « Windows a protégé votre ordinateur » : cliquez sur *Informations complémentaires*, puis *Exécuter quand même*.
>
> Pour vérifier un fichier, comparez la sortie de `certutil -hashfile Ferrox-Setup-1.0.0.exe SHA256` avec `SHA256SUMS.txt`.

### Fonctionnalités
- **Control**
  - RAM et CPU réels, mesurés comme dans le Gestionnaire des tâches, avec la répartition par processus et par onglet.
  - Limiteur de RAM et mise en veille des onglets.
  - Suspension des onglets en arrière-plan.
- **Sécurité**
  - Moteur WebView2 (Edge), mis à jour par Microsoft, avec bac à sable et SmartScreen.
  - Bloqueur de publicités et de traqueurs au niveau réseau, avec uBlock Origin Lite préinstallé.
  - Mode HTTPS uniquement.
  - Canal page → navigateur protégé par une clé secrète par onglet.
- **Interface**
  - 6 thèmes et couleur d'accent, animations fluides.
  - Page d'accueil avec des raccourcis modifiables et les vraies icônes des sites.
  - Menu clic droit Ferrox.
- **Pages intégrées** : Paramètres (une page par catégorie), Historique complet (recherche, suppression par visite, par site ou par période), Extensions (Chrome Web Store).
- **Navigation**
  - Restauration de session et onglets privés.
  - Vidéo en plein écran.
  - Raccourcis clavier : Ctrl+T/W/Maj+T/H/L, Ctrl+1…9, zoom, F11.
- **Recherche** : Brave Search (par défaut) ou Google.

### Prérequis
Windows 10 ou 11, 64 bits. Le moteur Microsoft Edge WebView2 est déjà présent sur ces systèmes ; l'installateur vérifie sa présence.
