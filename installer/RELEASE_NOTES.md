## Ferrox V2

Ferrox embarque maintenant son propre moteur **Chromium** (CEF) à la place de WebView2. Comme dans Opera GX, tous les processus du navigateur (pages, carte graphique, réseau) sont des processus **Ferrox** et sont regroupés sous Ferrox dans le Gestionnaire des tâches.

### Installation
- **`Ferrox-Setup-2.0.0.exe`** : installateur. Aucun droit administrateur n'est demandé. Il crée un raccourci dans le menu Démarrer et, en option, sur le bureau, et se désinstalle depuis « Applications installées ».
- **`Ferrox-2.0.0-windows.zip`** : contient l'installateur, une version portable (sans installation) et les empreintes SHA-256.

> Les fichiers ne sont pas signés numériquement. Au premier lancement, Windows peut afficher « Windows a protégé votre ordinateur » : cliquez sur *Informations complémentaires*, puis *Exécuter quand même*.
>
> Pour vérifier un fichier, comparez la sortie de `certutil -hashfile Ferrox-Setup-2.0.0.exe SHA256` avec `SHA256SUMS.txt`.

### Nouveautés
- Moteur Chromium intégré : rien à installer à côté, et le bac à sable de Chromium protège chaque processus de page.
- Le panneau Control mesure la RAM de chaque onglet à partir de son propre processus de page.
- Blocage réseau des pubs et des traqueurs directement dans le moteur, y compris dans les iframes.
- Les demandes d'autorisation (caméra, micro, position…) affichent une question par site quand le blocage des permissions est désactivé.
- Onglets privés gardés uniquement en mémoire.

### Changements à connaître
- Les extensions du Chrome Web Store (dont uBlock Origin Lite) ne sont plus disponibles : CEF ne les prend pas en charge. Le bloqueur intégré de Ferrox reste actif.
- SmartScreen (anti-phishing de Microsoft) n'est pas disponible dans Chromium embarqué.
- Le moteur n'est plus mis à jour par Windows : installez les nouvelles versions de Ferrox pour recevoir les correctifs de sécurité de Chromium.
- Les cookies et sessions de la V1 (profil WebView2) ne sont pas repris : il faut se reconnecter aux sites. Les paramètres, l'historique et les raccourcis de la page d'accueil sont conservés.

### Prérequis
Windows 10 ou 11, 64 bits.
