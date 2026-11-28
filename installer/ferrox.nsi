; =============================================================================
;  Ferrox — installateur Windows (NSIS 3, Modern UI 2)
;  Compilation :  makensis -DVERSION=0.4.2 installer/ferrox.nsi
;
;  - Installation par utilisateur (%LOCALAPPDATA%\Programs\Ferrox) :
;    aucun droit administrateur, pas de fenêtre UAC.
;  - Raccourci menu Démarrer (+ bureau en option), désinstallateur enregistré
;    dans « Applications installées ».
;  - Moteur Chromium (CEF) inclus : aucun prérequis à installer.
; =============================================================================

Unicode true
; LZMA « solide » avec un grand dictionnaire : le moteur Chromium (libcef.dll, ~300 Mo)
; se compresse bien mieux qu'avec le dictionnaire par défaut (8 Mo).
SetCompressor /SOLID /FINAL lzma
SetCompressorDictSize 128
SetDatablockOptimize on
ManifestDPIAware true

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!define APP       "Ferrox"
!define EXE       "Ferrox.exe"
!define PUBLISHER "Ferrox"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP}"
OutFile "..\dist\Ferrox-Setup-${VERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
InstallDirRegKey HKCU "Software\${APP}" "InstallDir"
RequestExecutionLevel user
BrandingText "${APP} ${VERSION}"

; --- Informations affichées dans les propriétés de setup.exe
VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1036 "ProductName"     "${APP}"
VIAddVersionKey /LANG=1036 "FileDescription" "Installation de ${APP}"
VIAddVersionKey /LANG=1036 "CompanyName"     "${PUBLISHER}"
VIAddVersionKey /LANG=1036 "FileVersion"     "${VERSION}"
VIAddVersionKey /LANG=1036 "ProductVersion"  "${VERSION}"
VIAddVersionKey /LANG=1036 "LegalCopyright"  "Licence MIT"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

; --- Apparence
!define MUI_ICON   "..\assets\ferrox.ico"
!define MUI_UNICON "..\assets\ferrox.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP   "welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "welcome.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP   "header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP "header.bmp"
!define MUI_ABORTWARNING
!define MUI_ABORTWARNING_TEXT "Voulez-vous vraiment annuler l'installation de ${APP} ?"
!define MUI_COMPONENTSPAGE_NODESC

; --- Pages d'installation
!define MUI_WELCOMEPAGE_TITLE "Bienvenue dans l'installation de ${APP}"
!define MUI_WELCOMEPAGE_TEXT "${APP} est un navigateur web léger et sécurisé, écrit en Rust.$\r$\n$\r$\n\
  • Moteur Chromium intégré, isolé dans un bac à sable$\r$\n\
  • Bloqueur de publicités et de traqueurs intégré$\r$\n\
  • Limiteur de RAM et mise en veille des onglets$\r$\n\
  • Aucun droit administrateur nécessaire$\r$\n$\r$\n\
  Cliquez sur Suivant pour continuer."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TITLE "${APP} est installé"
!define MUI_FINISHPAGE_TEXT "Vous trouverez ${APP} dans le menu Démarrer.$\r$\n$\r$\nPour le désinstaller : Paramètres Windows → Applications → Applications installées."
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Lancer ${APP} maintenant"
!insertmacro MUI_PAGE_FINISH

; --- Pages de désinstallation
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "French"

; =============================================================================
;  Fonctions utilitaires
; =============================================================================

; Ferme Ferrox s'il est ouvert (nécessaire pour remplacer ses fichiers).
!macro CLOSE_FERROX un
Function ${un}CloseFerrox
  ; code de retour 0 = Ferrox trouvé parmi les processus
  nsExec::Exec 'cmd /c tasklist /FI "IMAGENAME eq ${EXE}" /NH | find /I "${EXE}" >nul'
  Pop $0
  ${If} $0 == 0
    MessageBox MB_OKCANCEL|MB_ICONINFORMATION "${APP} est ouvert. Il va être fermé pour continuer.$\r$\n$\r$\nVos onglets seront restaurés si l'option « Reprendre là où je m'étais arrêté » est activée." IDOK +2
    Abort
    nsExec::Exec 'taskkill /IM ${EXE} /T'
    Sleep 1500
    nsExec::Exec 'taskkill /IM ${EXE} /T /F'
    Sleep 500
  ${EndIf}
FunctionEnd
!macroend
!insertmacro CLOSE_FERROX ""
!insertmacro CLOSE_FERROX "un."

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "${APP} nécessite Windows 10 ou 11 en 64 bits."
    Abort
  ${EndIf}
FunctionEnd

; =============================================================================
;  Sections
; =============================================================================

Section "${APP} (obligatoire)" SecMain
  SectionIn RO
  Call CloseFerrox

  SetOutPath "$INSTDIR"
  ; Ferrox.exe, ferrox.dll et le moteur Chromium (libcef.dll, ressources, langues…)
  File /r "..\dist\Ferrox\*.*"
  ; Icône au nom versionné : Windows n'a aucune image en cache pour ce nouveau fichier,
  ; les raccourcis affichent donc forcément le logo de cette version.
  Delete "$INSTDIR\Ferrox-*.ico"
  File "/oname=Ferrox-${VERSION}.ico" "..\assets\ferrox.ico"
  WriteUninstaller "$INSTDIR\Desinstaller.exe"

  ; Raccourci menu Démarrer
  Delete "$SMPROGRAMS\${APP}.lnk"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\Ferrox-${VERSION}.ico" 0 SW_SHOWNORMAL "" "Navigateur web léger et sécurisé"

  ; « Exécuter » (Win+R) → ferrox
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\${EXE}" "" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\${EXE}" "Path" "$INSTDIR"

  ; Désinstallation depuis « Applications installées »
  WriteRegStr HKCU "Software\${APP}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\Ferrox-${VERSION}.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Desinstaller.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Desinstaller.exe" /S'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ; Demande à Windows de rafraîchir son cache d'icônes (sinon l'ancien logo peut rester
  ; affiché dans la barre des tâches et sur les raccourcis après une mise à jour).

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "Raccourci sur le bureau" SecDesktop
  Delete "$DESKTOP\${APP}.lnk"
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\Ferrox-${VERSION}.ico" 0 SW_SHOWNORMAL "" "Navigateur web léger et sécurisé"
SectionEnd

; =============================================================================
;  Désinstallation
; =============================================================================

; Une fois TOUS les raccourcis créés : demande à Windows de rafraîchir ses icônes.
Function .onInstSuccess
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x1000, p 0, p 0)'
  ; installateur 32 bits : « Sysnative » donne accès au vrai System32 de Windows 64 bits
  IfFileExists "$WINDIR\Sysnative\ie4uinit.exe" 0 +3
    nsExec::Exec '"$WINDIR\Sysnative\ie4uinit.exe" -show'
    Goto +2
    nsExec::Exec '"$SYSDIR\ie4uinit.exe" -show'
FunctionEnd

Section "Uninstall"
  Call un.CloseFerrox

  ; Uniquement nos fichiers (jamais de suppression récursive du dossier d'installation)
  ; liste générée à la compilation à partir des fichiers réellement installés
  !include "files.nsh"
  Delete "$INSTDIR\Desinstaller.exe"
  Delete "$INSTDIR\Ferrox-*.ico"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"

  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\${EXE}"
  DeleteRegKey HKCU "Software\${APP}"

  ; Données personnelles : seulement si l'utilisateur le demande (non par défaut)
  ${IfNot} ${Silent}
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Supprimer aussi vos données ${APP} ?$\r$\n$\r$\nParamètres, historique, session et cookies.$\r$\nChoisissez « Non » pour les retrouver si vous réinstallez ${APP}." IDNO keep
    RMDir /r "$APPDATA\ferrox"
    RMDir /r "$LOCALAPPDATA\ferrox"
    keep:
  ${EndIf}
SectionEnd
