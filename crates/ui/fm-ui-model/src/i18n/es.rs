//! Spanish catalog.
//!
//! Broadcast abbreviations that Spanish-speaking operators read as-is on
//! hardware (`PGM`, `PVW`, `REC`, `T-bar`, `Stinger`, `FIFO`) stay
//! untranslated; full words are translated (`Previo`, `Programa`, `Corte`).
//! Status words that describe an input use the feminine (`entrada`).

use super::{Entry, Msg, plural, t};

pub(super) const CATALOG: &[(Msg, Entry)] = &[
    // Shared words and templates.
    (Msg::Unavailable, t("No disponible: {reason}")),
    (Msg::None, t("ninguna")),
    (Msg::Locked, t("BLOQUEADO")),
    (Msg::Unlocked, t("DESBLOQUEADO")),
    (Msg::Pending, t("PENDIENTE")),
    (Msg::InUse, t("EN USO")),
    (Msg::Live, t("VIVO")),
    (Msg::Recording, t("GRABANDO")),
    (Msg::Stopped, t("DETENIDO")),
    (Msg::EditsLocked, t("EDICIÓN BLOQUEADA")),
    (Msg::Undo, t("Deshacer")),
    (Msg::Redo, t("Rehacer")),
    (Msg::InputBank, t("Banco de entradas")),
    (Msg::SelectOnPreview, t("Seleccionar en Previo")),
    (Msg::RenameEllipsis, t("Renombrar…")),
    (Msg::AddInput, t("Añadir entrada")),
    (Msg::AddInputEllipsis, t("Añadir entrada…")),
    (Msg::FadeToBlack, t("Fundido a negro")),
    (Msg::NoSynchronizedShow, t("Ningún show sincronizado")),
    (Msg::KeyboardShortcuts, t("Atajos de teclado")),
    // Connection lifecycle.
    (Msg::ConnLaunching, t("INICIANDO")),
    (Msg::ConnConnecting, t("CONECTANDO")),
    (Msg::ConnSynchronizing, t("SINCRONIZANDO")),
    (Msg::ConnReady, t("CONTROL LISTO")),
    (Msg::ConnBackoff, t("ESPERA DE REINTENTO")),
    (Msg::ConnDisconnected, t("DESCONECTADO")),
    (Msg::ConnFailed, t("FALLIDO")),
    (Msg::ConnProtocolMismatch, t("PROTOCOLO INCOMPATIBLE")),
    // Why a control is disabled.
    (
        Msg::ReasonProtocol,
        t("el daemon no seleccionó el protocolo actual"),
    ),
    (
        Msg::ReasonConnectionFailed,
        t("la conexión de control falló"),
    ),
    (Msg::ReasonNoConnection, t("no hay conexión de control")),
    (
        Msg::ReasonNotReady,
        t("la conexión de control aún no está lista"),
    ),
    (
        Msg::ReasonNotSynchronized,
        t("el estado del proyecto aún no está sincronizado"),
    ),
    (
        Msg::ReasonNoSelectPreview,
        t("esta sesión no tiene el permiso select_preview"),
    ),
    (
        Msg::ReasonNoTransition,
        t("esta sesión no tiene el permiso transition"),
    ),
    (
        Msg::ReasonNoControlAudio,
        t("esta sesión no tiene el permiso control_audio"),
    ),
    (
        Msg::ReasonNoEditProject,
        t("esta sesión no tiene el permiso edit_project"),
    ),
    (
        Msg::ReasonManualInFlight,
        t("hay una transición manual con T-bar en curso"),
    ),
    (
        Msg::ReasonStateDisallows,
        t("el estado actual no lo permite"),
    ),
    (
        Msg::ReasonProjectLocked,
        t("el proyecto está bloqueado contra ediciones"),
    ),
    (
        Msg::ReasonInputLocked,
        t("esta entrada está bloqueada contra ediciones"),
    ),
    (
        Msg::ReasonNothingToUndo,
        t("no hay ninguna edición del proyecto que deshacer"),
    ),
    (
        Msg::ReasonNothingToRedo,
        t("no hay ninguna edición del proyecto que rehacer"),
    ),
    (
        Msg::ReasonNeighbourLocked,
        t("la entrada vecina está bloqueada contra movimientos"),
    ),
    (
        Msg::ReasonNoPreviewInput,
        t("no hay ninguna entrada seleccionada en Previo"),
    ),
    (
        Msg::ReasonPreviewFirst,
        t("la entrada de Previo ya es la primera"),
    ),
    (
        Msg::ReasonPreviewLast,
        t("la entrada de Previo ya es la última"),
    ),
    (Msg::ReasonInputFirst, t("esta entrada ya es la primera")),
    (Msg::ReasonInputLast, t("esta entrada ya es la última")),
    (Msg::ReasonShowHasNoInputs, t("el show no tiene entradas")),
    (
        Msg::ReasonNoManualActive,
        t("no hay ninguna transición manual activa"),
    ),
    (
        Msg::ReasonRecordingPending,
        t("un comando de grabación anterior sigue pendiente"),
    ),
    (
        Msg::ReasonNoStreamTargets,
        t("el show no tiene destinos de transmisión configurados"),
    ),
    (
        Msg::ReasonNoStingerConfigured,
        t("no hay ninguna ranura de Stinger configurada"),
    ),
    (
        Msg::ReasonStingerSlotNotConfigured,
        t("esta ranura de Stinger no está configurada"),
    ),
    (
        Msg::ReasonSidePanelHidden,
        t("el panel lateral está oculto"),
    ),
    (
        Msg::ReasonInputLimit,
        t("el show ya tiene el máximo de 256 entradas"),
    ),
    (Msg::ReasonUnknownInput, t("la entrada ya no existe")),
    (
        Msg::ReasonRemovalPending,
        t("ya hay una eliminación pendiente de esta entrada"),
    ),
    (Msg::ReasonOnProgram, t("esta entrada está en Programa")),
    (Msg::ReasonOnPreview, t("esta entrada está en Previo")),
    (
        Msg::ReasonInTransition,
        t("esta entrada está en la transición manual activa"),
    ),
    (
        Msg::ReasonStingerMedia,
        t("esta entrada es el medio de una ranura de Stinger"),
    ),
    (
        Msg::ReasonOverlayOnAir,
        t("esta entrada está al aire en un overlay"),
    ),
    (
        Msg::ReasonOverlayQueued,
        t("esta entrada está en la cola de un overlay"),
    ),
    (Msg::ReasonNameEmpty, t("el nombre está vacío")),
    (Msg::ReasonNameTooLong, t("el nombre es demasiado largo")),
    (
        Msg::ReasonNameTooLongBytes,
        t("el nombre supera los 128 bytes"),
    ),
    (Msg::ReasonNameTaken, t("otra entrada ya usa este nombre")),
    (Msg::ReasonNameUnchanged, t("el nombre no ha cambiado")),
    // Top bar.
    (Msg::UntitledShow, t("Show sin título")),
    (
        Msg::ShowNameHover,
        t("El nombre del show aparece cuando el estado del proyecto se sincroniza"),
    ),
    (
        Msg::ConnectionPillHover,
        t("Ciclo de vida de la conexión de control"),
    ),
    (Msg::RevisionShort, t("REV {revision}")),
    (Msg::RevisionHover, t("Revisión replicada del proyecto")),
    (Msg::PendingPill, t("{count} PENDIENTES")),
    (
        Msg::PadlockClickUnlock,
        t("Haga clic para desbloquear las ediciones del proyecto."),
    ),
    (
        Msg::PadlockClickLock,
        t(
            "Haga clic para bloquear las ediciones del proyecto; la conmutación en vivo sigue disponible.",
        ),
    ),
    (Msg::ClockHover, t("Reloj de pared (UTC) · {date}")),
    (Msg::BadgeRec, t("REC")),
    (
        Msg::LiveHoverLive,
        t("Un destino de transmisión informa que está en vivo"),
    ),
    (
        Msg::LiveHoverDesired,
        t("Un destino de transmisión debe estar activo pero no informa que esté en vivo"),
    ),
    (
        Msg::LiveHoverIdle,
        t("Ningún destino de transmisión debe estar activo"),
    ),
    (
        Msg::RecHoverRecording,
        t("Una ranura de grabación debe estar grabando"),
    ),
    (
        Msg::RecHoverIdle,
        t("Ninguna ranura de grabación debe estar grabando"),
    ),
    // Edit history and padlocks.
    (Msg::UndoEditAt, t("Deshacer edición en REV {revision}")),
    (Msg::RedoEditAt, t("Rehacer edición en REV {revision}")),
    (
        Msg::UndoHoverAt,
        t("Deshacer la edición del proyecto en REV {revision}"),
    ),
    (
        Msg::RedoHoverAt,
        t("Rehacer la edición del proyecto en REV {revision}"),
    ),
    (Msg::HistoryEmpty, t("historial vacío")),
    (
        Msg::HistorySteps,
        plural(
            "{count} paso en el historial compartido",
            "{count} pasos en el historial compartido",
        ),
    ),
    (
        Msg::PadlockHoverLocked,
        t(
            "Las ediciones del proyecto están bloqueadas; la conmutación en vivo sigue funcionando. Haga clic para desbloquear.",
        ),
    ),
    (
        Msg::PadlockHoverUnlocked,
        t(
            "Las ediciones del proyecto están desbloqueadas. Haga clic para bloquearlas; la conmutación en vivo sigue disponible.",
        ),
    ),
    (
        Msg::PadlockMenuUnlock,
        t("Desbloquear ediciones del proyecto"),
    ),
    (Msg::PadlockMenuLock, t("Bloquear ediciones del proyecto")),
    (
        Msg::InputsLockedChip,
        plural("{count} BLOQUEADA", "{count} BLOQUEADAS"),
    ),
    (
        Msg::InputsLockedDetail,
        plural(
            "{count} entrada está bloqueada contra cambio de nombre, movimiento y eliminación.",
            "{count} entradas están bloqueadas contra cambio de nombre, movimiento y eliminación.",
        ),
    ),
    (
        Msg::ProjectEditsLockedDetail,
        t(
            "Las ediciones del proyecto están bloqueadas: renombrar, mover, añadir y quitar entradas, configurar ranuras de Stinger, deshacer y rehacer están bloqueados. La conmutación, las transiciones, el audio, los overlays y las salidas siguen funcionando.",
        ),
    ),
    (
        Msg::ProjectEditsUnlocked,
        t("Las ediciones del proyecto están desbloqueadas."),
    ),
    // Status bar.
    (
        Msg::PendingCount,
        plural("{count} pendiente", "{count} pendientes"),
    ),
    (Msg::StatusError, t("ERROR | {message}")),
    (Msg::StatusNotice, t("AVISO | {message}")),
    (
        Msg::StatusUncertain,
        plural(
            "{count} comando con resultado incierto; el último {command}",
            "{count} comandos con resultado incierto; el último {command}",
        ),
    ),
    (Msg::StatusIdleReady, t("Listo")),
    (
        Msg::StatusIdleProtocol,
        t("El otro extremo no seleccionó el protocolo actual"),
    ),
    (Msg::StatusIdleFailed, t("La conexión falló")),
    (Msg::StatusIdleDisconnected, t("Sin conexión de control")),
    (
        Msg::StatusIdleWaiting,
        t("Esperando el estado sincronizado"),
    ),
    (
        Msg::StatusBarKeyHints,
        t("1–8 PVW   C CORTE   F FUNDIDO   B FTB   ESC CANCELAR T-BAR   ·   ? TODOS LOS ATAJOS"),
    ),
    (Msg::StatusBarKeys, t("TECLAS")),
    (Msg::StatusBarHelpHint, t("? ATAJOS")),
    // Status bar health strip.
    (Msg::HealthFeedLive, t("SEÑAL PGM")),
    (Msg::HealthFeedStale, t("SEÑAL PGM {age}")),
    (Msg::HealthFeedNone, t("SIN SEÑAL PGM")),
    (
        Msg::HealthFeedHover,
        t(
            "Vista del monitor de Programa: fotograma de {width}×{height} recibido hace {age}. Pasa a ámbar tras 1 s sin un fotograma nuevo.",
        ),
    ),
    (
        Msg::HealthFeedNoneHover,
        t("No ha llegado ningún fotograma del monitor de Programa desde el daemon"),
    ),
    (Msg::HealthMetersLive, t("MEDIDORES")),
    (Msg::HealthMetersStale, t("MEDIDORES OBSOLETOS")),
    (Msg::HealthMetersNone, t("SIN MEDIDORES")),
    (
        Msg::HealthMetersHover,
        t(
            "Medidores de audio: el último mensaje llegó hace {age}. Pasa a ámbar tras 1 s sin un mensaje nuevo.",
        ),
    ),
    (Msg::HealthStream, t("TX {rate}")),
    (Msg::HealthStreamUnknown, t("TX —")),
    (
        Msg::HealthStreamHover,
        t("Emisiones: {live} de {total} en vivo · {rate} · {dropped} pares A/V descartados"),
    ),
    (
        Msg::HealthRateMeasured,
        t("{rate} multiplexados en total, medidos entre muestras de transporte"),
    ),
    (
        Msg::HealthRateUnknown,
        t("sin muestras de transporte recientes para medir la tasa"),
    ),
    (Msg::HealthDropped, t("DESCARTES {count}")),
    (
        Msg::HealthDroppedHover,
        plural(
            "{count} par de audio/vídeo descartado por las salidas de emisión desde que se iniciaron",
            "{count} pares de audio/vídeo descartados por las salidas de emisión desde que se iniciaron",
        ),
    ),
    (Msg::HealthDisk, t("DISCO HLS {free}")),
    (
        Msg::HealthDiskHover,
        t(
            "Menor espacio libre en el sistema de archivos de un directorio de salida HLS local: {free}. Ámbar por debajo de 2 GB, rojo por debajo de 512 MB.",
        ),
    ),
    (Msg::BitrateKbps, t("{value} kb/s")),
    (Msg::BitrateMbps, t("{value} Mb/s")),
    // Toasts.
    (Msg::ToastNotice, t("AVISO")),
    (Msg::ToastError, t("ERROR")),
    (Msg::FtbOnKey, t("FTB  ACTIVO")),
    (Msg::ThumbPgmTapSim, t("VISTA PGM · SIM")),
    (Msg::ThumbPvwTapSim, t("VISTA PVW · SIM")),
    (Msg::ThumbPgmTap, t("VISTA PGM")),
    (Msg::ThumbPvwTap, t("VISTA PVW")),
    (Msg::ToastUncertain, t("RESULTADO INCIERTO")),
    (Msg::ToastDone, t("HECHO")),
    (
        Msg::ToastDismissHover,
        t("Descartar; la barra de estado conserva el último mensaje"),
    ),
    (
        Msg::ToastUncertainCommand,
        t(
            "El comando {command} pudo aplicarse o no (recibo del servidor {receipt}). Revise Programa antes de reintentar.",
        ),
    ),
    (
        Msg::QueuedCommands,
        plural(
            "{count} comando en la cola del operador",
            "{count} comandos en la cola del operador",
        ),
    ),
    (
        Msg::ToastAdding,
        t("Añadiendo «{name}» como entrada {input}…"),
    ),
    (
        Msg::ToastRemoving,
        t("Quitando «{name}» (entrada {input})…"),
    ),
    (
        Msg::ToastAdded,
        t(
            "Se añadió «{name}» como entrada {input} en la posición {position}. Deshacer la quita de nuevo.",
        ),
    ),
    (
        Msg::ToastRemoved,
        t("Se quitó «{name}» (entrada {input}) con su canal del mezclador."),
    ),
    (
        Msg::ToastAddNotApplied,
        t("No se aplicó la adición de «{name}»: {detail}"),
    ),
    (
        Msg::ToastRemovalNotApplied,
        t("No se aplicó la eliminación de «{name}»: {detail}"),
    ),
    (
        Msg::ToastAddNotConfirmed,
        t(
            "La adición de «{name}» no se confirmó tras {seconds} s: {detail}. Revise el banco de entradas.",
        ),
    ),
    (
        Msg::ToastRemovalNotConfirmed,
        t(
            "La eliminación de «{name}» no se confirmó tras {seconds} s: {detail}. Revise el banco de entradas.",
        ),
    ),
    (Msg::NoDaemonResult, t("sin resultado del daemon")),
    // Menu bar.
    (Msg::MenuFile, t("Archivo")),
    (Msg::MenuEdit, t("Editar")),
    (Msg::MenuView, t("Ver")),
    (Msg::MenuInputs, t("Entradas")),
    (Msg::MenuOutput, t("Salida")),
    (Msg::MenuHelp, t("Ayuda")),
    (Msg::FileShowHeading, t("Show")),
    (
        Msg::FileRevisionInputs,
        plural(
            "Revisión {revision} · {count} entrada",
            "Revisión {revision} · {count} entradas",
        ),
    ),
    (
        Msg::FileSavedByDaemon,
        t("El daemon guarda el show tras cada edición aceptada."),
    ),
    (Msg::FileQuit, t("Salir de FreeMix Studio")),
    (
        Msg::FileQuitHover,
        t("Cierra esta ventana; un daemon supervisado se detiene con ella"),
    ),
    (Msg::EditPreviewInputHeading, t("Entrada en Previo")),
    (Msg::MoveEarlier, t("Mover antes")),
    (Msg::MoveLater, t("Mover después")),
    (Msg::HistoryHeading, t("Historial")),
    (Msg::ViewPanels, t("Paneles")),
    (
        Msg::ViewInputBankHover,
        t("Muestra u oculta el banco de entradas"),
    ),
    (Msg::ViewAudioMixer, t("Mezclador de audio")),
    (
        Msg::ViewAudioMixerHover,
        t("Muestra u oculta el mezclador de audio"),
    ),
    (Msg::ViewSidePanel, t("Panel lateral")),
    (
        Msg::ViewSidePanelHover,
        t("Muestra u oculta grabación, transmisiones, overlays, stingers y títulos"),
    ),
    (Msg::ViewStatusBar, t("Barra de estado")),
    (
        Msg::ViewStatusBarHover,
        t("Muestra u oculta la barra de estado inferior"),
    ),
    (Msg::ViewSidePanelTab, t("Pestaña del panel lateral")),
    (Msg::TabOverlays, t("Overlays")),
    (Msg::TabStingers, t("Stingers")),
    (Msg::TabTitles, t("Títulos")),
    (Msg::ViewResetLayout, t("Restablecer diseño")),
    (
        Msg::ViewResetLayoutDisabled,
        t("el diseño ya es el predeterminado"),
    ),
    (
        Msg::ViewResetLayoutHover,
        t("Mostrar todos los paneles y restaurar el ancho predeterminado del panel lateral"),
    ),
    (
        Msg::ViewLayoutRemembered,
        t(
            "Los paneles y el ancho del panel lateral (arrastre su borde) se recuerdan en esta consola",
        ),
    ),
    (Msg::ViewLanguage, t("Idioma")),
    (
        Msg::ViewLanguageHover,
        t("Idioma de la consola; local a esta consola y nunca enviado al daemon"),
    ),
    (Msg::ViewClock12h, t("Reloj de 12 horas")),
    (
        Msg::ViewClock12hHover,
        t("Muestra el reloj de la barra superior con a. m./p. m. en lugar de 24 horas"),
    ),
    (Msg::MenuPreferences, t("Preferencias…")),
    (
        Msg::MenuPreferencesHover,
        t("Idioma, reloj, escala de la interfaz y contraste de esta consola"),
    ),
    (Msg::PrefsTitle, t("Preferencias")),
    (Msg::PrefsClock, t("Reloj")),
    (Msg::PrefsClock24h, t("24 horas")),
    (Msg::PrefsClock12h, t("12 horas (a. m./p. m.)")),
    (
        Msg::PrefsClockHover,
        t("Cómo muestra la hora el reloj de la barra superior"),
    ),
    (Msg::PrefsDisplay, t("Pantalla")),
    (Msg::PrefsUiScale, t("Escala de la interfaz")),
    (
        Msg::PrefsUiScaleHover,
        t("Tamaño de todo el texto y los controles; se aplica al instante"),
    ),
    (Msg::PrefsPercent, t("{percent} %")),
    (
        Msg::PrefsUiScaleKeys,
        t("Ctrl+= y Ctrl+- cambian la escala; Ctrl+0 vuelve al 100 %"),
    ),
    (Msg::PrefsHighContrast, t("Tema de alto contraste")),
    (
        Msg::PrefsHighContrastHover,
        t(
            "Texto y bordes más claros sobre paneles más oscuros; el rojo, verde y ámbar de tally conservan su significado",
        ),
    ),
    (
        Msg::PrefsSavedNote,
        t(
            "Se guardan en esta consola en studio.conf y nunca se envían al daemon. Una opción --lang o --clock prevalece en su sesión sin guardarse.",
        ),
    ),
    (Msg::PrefsReset, t("Restablecer valores predeterminados")),
    (
        Msg::PrefsResetHover,
        t("Idioma del sistema, reloj de 24 horas, escala del 100 % y tema estándar"),
    ),
    (
        Msg::PrefsResetDisabled,
        t("todas las preferencias ya tienen su valor predeterminado"),
    ),
    (Msg::PrefsDone, t("Listo")),
    (Msg::PrefsDoneHover, t("Cerrar Preferencias  [Esc]")),
    (Msg::PrefsUiScaleToast, t("Escala de la interfaz {percent}")),
    (Msg::ShortcutPreferences, t("Abrir o cerrar Preferencias")),
    (Msg::ShortcutUiScale, t("Ampliar o reducir la interfaz")),
    (
        Msg::ShortcutUiScaleReset,
        t("Devolver la interfaz a la escala del 100 %"),
    ),
    (
        Msg::OverlayDetailsToggleHover,
        t(
            "Mostrar u ocultar los detalles de este canal: actualizar, cola, transición, posición, borde y salidas",
        ),
    ),
    (Msg::NoInputs, t("Sin entradas")),
    (Msg::CutDirectlyToProgram, t("Cortar directo a Programa")),
    (Msg::CatalogueHeading, t("Catálogo")),
    (
        Msg::AddInputMenuHover,
        t("Añade al final del banco una entrada de color sólido o barras de color"),
    ),
    (Msg::RemoveInput, t("Quitar entrada")),
    (
        Msg::RemoveInputHover,
        t("Quitar {name} (pide confirmación)"),
    ),
    (
        Msg::RemoveInputMenuHover,
        t("Quita una entrada que no esté al aire ni en uso"),
    ),
    (Msg::TransitionHeading, t("Transición · {frames} fr")),
    (Msg::TransitionCut, t("Corte")),
    (Msg::TransitionFade, t("Fundido")),
    (Msg::TransitionWipe, t("Cortina")),
    (Msg::TransitionAlphaFade, t("Fundido alfa")),
    (Msg::TransitionSlide, t("Deslizar")),
    (Msg::TransitionZoom, t("Zoom")),
    (Msg::ReturnToLive, t("Volver al vivo")),
    (Msg::ManualTBarHeading, t("T-bar manual")),
    (Msg::CommitTransition, t("Completar transición")),
    (Msg::CancelTransition, t("Cancelar transición")),
    (Msg::RecordingHeading, t("Grabación")),
    (Msg::StartRecorder, t("Iniciar grabador {slot}")),
    (Msg::StopRecorder, t("Detener grabador {slot}")),
    (
        Msg::RecorderHover,
        t("{label}: ranura {slot} de grabación de Programa"),
    ),
    (Msg::StreamsHeading, t("Transmisiones")),
    (Msg::NoStreamTargets, t("Sin destinos de transmisión")),
    (Msg::StartStream, t("Iniciar {name}")),
    (Msg::StopStream, t("Detener {name}")),
    (
        Msg::AssignStingerMedia,
        t("Asignar Previo como medio de stinger"),
    ),
    (
        Msg::AssignStingerMediaHover,
        t(
            "Usa la entrada de Previo como medio de una ranura; los demás ajustes de la ranura se conservan",
        ),
    ),
    (
        Msg::AssignStingerSlotHover,
        t("Asignar la entrada de Previo como medio del Stinger S{slot}"),
    ),
    (Msg::StingerSlot, t("Ranura S{slot}")),
    (Msg::ClearStingerSlot, t("Vaciar ranura de stinger")),
    (Msg::ClearStingerSlotHover, t("Vaciar el Stinger S{slot}")),
    (Msg::StingerSlotsEllipsis, t("Ranuras de stinger…")),
    (
        Msg::StingerSlotsHover,
        t("Abre la pestaña Stingers para elegir cualquier entrada como medio de una ranura"),
    ),
    (Msg::AboutStudio, t("Acerca de FreeMix Studio")),
    (Msg::About, t("Acerca de")),
    (Msg::AboutVersion, t("Versión {version}")),
    (
        Msg::AboutDescription,
        t(
            "Consola de operación nativa para el motor de producción en vivo freemixd. La consola muestra el estado de control replicado y envía comandos; el daemon gestiona los medios, la persistencia y las salidas.",
        ),
    ),
    (Msg::AboutLicense, t("Licencia: {license}")),
    (Msg::CloseEsc, t("Cerrar  [Esc]")),
    (Msg::KeysActive, t("Teclas activas")),
    (Msg::KeysInactive, t("Teclas inactivas: {reason}")),
    (
        Msg::ShortcutsIntro,
        t(
            "Las teclas solo actúan cuando la consola está lista, ningún campo de texto tiene el foco y la sesión tiene el permiso correspondiente.",
        ),
    ),
    (
        Msg::ShortcutsFooter,
        t("Esc o un clic fuera cierra esta referencia"),
    ),
    (Msg::ShortcutGroupSwitching, t("Conmutación")),
    (Msg::ShortcutGroupTransitions, t("Transiciones")),
    (Msg::ShortcutGroupEditing, t("Edición")),
    (
        Msg::ShortcutGroupInputBank,
        t("Banco de entradas (puntero y foco)"),
    ),
    (
        Msg::ShortcutSelectPreview,
        t("Seleccionar la entrada 1–8 en Previo"),
    ),
    (Msg::ShortcutCut, t("Cortar Previo a Programa")),
    (
        Msg::ShortcutFade,
        t("Fundir Previo a Programa (duración compartida)"),
    ),
    (Msg::ShortcutFadeToBlack, t("Alternar el fundido a negro")),
    (
        Msg::ShortcutFavorite,
        t("Lanzar la transición favorita 1 a 4"),
    ),
    (
        Msg::ShortcutCancelManual,
        t("Cancelar la transición manual con T-bar activa"),
    ),
    (
        Msg::ShortcutUndo,
        t("Deshacer la última edición del proyecto (historial compartido)"),
    ),
    (
        Msg::ShortcutRedo,
        t("Rehacer la última edición deshecha del proyecto"),
    ),
    (
        Msg::ShortcutTileClick,
        t("Seleccionar la entrada en Previo"),
    ),
    (
        Msg::ShortcutTileDoubleClick,
        t("Seleccionar en Previo y cortar a Programa"),
    ),
    (
        Msg::ShortcutTileMenu,
        t(
            "Menú de la entrada: Previo, Corte, Duplicar, Reemplazar fuente, Categoría, Color, Pausa, Cerrar, Renombrar, Mover, Quitar",
        ),
    ),
    (
        Msg::ShortcutTileFocus,
        t("Enfocar un mosaico y seleccionarlo en Previo"),
    ),
    (Msg::KeyClick, t("Clic")),
    (Msg::KeyDoubleClick, t("Doble clic")),
    (Msg::KeyDoubleClickShort, t("Doble clic")),
    (Msg::KeyRightClick, t("Clic derecho")),
    (Msg::KeyTabEnter, t("Tab · Intro")),
    (Msg::ShortcutGroupConsole, t("Consola")),
    (
        Msg::ShortcutHelp,
        t("Mostrar u ocultar esta referencia de teclado"),
    ),
    (
        Msg::ShortcutCloseDialog,
        t("Cerrar el menú, diálogo o referencia abiertos"),
    ),
    (Msg::KeyHelp, t("? · F1")),
    // Output rows.
    (Msg::StatusNoState, t("SIN ESTADO")),
    (Msg::StreamStarting, t("INICIANDO")),
    (Msg::StreamReconnecting, t("RECONECTANDO")),
    (Msg::StreamCongested, t("CONGESTIONADO")),
    (Msg::StreamNotLive, t("NO EN VIVO")),
    (Msg::StreamOff, t("APAGADO")),
    (Msg::StreamUnavailable, t("NO DISPONIBLE")),
    (Msg::RowNotSynchronized, t("Sin sincronizar")),
    (Msg::RowCommandPending, t("Comando pendiente")),
    (Msg::RowRecordingProgram, t("Grabando Programa")),
    (Msg::RowProgramIdle, t("Programa · inactivo")),
    (
        Msg::RowStreamCounters,
        plural(
            "{bytes} enviados · {count} descartado",
            "{bytes} enviados · {count} descartados",
        ),
    ),
    (Msg::RowSegments, plural("{count} seg.", "{count} seg.")),
    (
        Msg::RowRequestedNoSample,
        t("Solicitado · sin muestra de transporte"),
    ),
    (Msg::RowDesiredOff, t("Apagado por decisión")),
    (Msg::StingerStateUnavailable, t("ESTADO NO DISPONIBLE")),
    (Msg::StingerUnconfigured, t("SIN CONFIGURAR")),
    (Msg::StingerNotRequested, t("NO SOLICITADO")),
    (Msg::StingerReady, t("LISTO")),
    (Msg::StingerMissing, t("FALTA")),
    // Input bank.
    (Msg::SourcesUnknown, t("— FUENTES")),
    (
        Msg::SourcesCount,
        plural("{count} FUENTE", "{count} FUENTES"),
    ),
    (
        Msg::BankGestureHint,
        t("clic PVW · doble clic CORTE · clic derecho más"),
    ),
    (Msg::NoInputState, t("Sin estado de entradas")),
    (
        Msg::NoInputStateDetail,
        t("Las fuentes aparecen cuando el estado del proyecto se sincroniza."),
    ),
    (
        Msg::ThumbnailNone,
        t("Miniatura: ninguna (solo se reciben fotogramas de las tomas de Previo y Programa)"),
    ),
    (
        Msg::ThumbnailProgram,
        t("Miniatura: fotograma actual de la toma de Programa"),
    ),
    (
        Msg::ThumbnailPreview,
        t("Miniatura: fotograma actual de la toma de Previo"),
    ),
    (
        Msg::TileGestures,
        t("Clic: seleccionar Previo{shortcut} · Doble clic: cortar a Programa · Clic derecho: más"),
    ),
    (
        Msg::TileMoreHover,
        t(
            "Más: Previo, Corte, Duplicar, Categoría, Color, Pausa, Cerrar, Renombrar, Mover, Quitar",
        ),
    ),
    (Msg::CutKey, t("CORTE")),
    (
        Msg::TileCutHover,
        t("Selecciona esta entrada en Previo y luego la corta a Programa  [doble clic]"),
    ),
    (Msg::CutToProgram, t("Cortar a Programa")),
    (Msg::MoveLeft, t("Mover a la izquierda")),
    (Msg::MoveRight, t("Mover a la derecha")),
    (Msg::RemoveEllipsis, t("Quitar…")),
    (
        Msg::NotePreviewCutUnavailable,
        t("Previo y corte no disponibles: {reason}"),
    ),
    (Msg::NoteCutUnavailable, t("Corte no disponible: {reason}")),
    (
        Msg::NoteEditUnavailable,
        t("Renombrar, mover y quitar no disponibles: {reason}"),
    ),
    (
        Msg::NoteRemoveUnavailable,
        t("Quitar no disponible: {reason}"),
    ),
    (Msg::ApplyKey, t("APLICAR")),
    (Msg::CancelKey, t("CANCELAR")),
    (Msg::RenameApplyHover, t("Renombrar esta entrada  [Intro]")),
    (Msg::RenameCancelHover, t("Descartar este cambio de nombre")),
    (
        Msg::AddTileHover,
        t("Añadir entrada…\nUn color sólido o barras de color, al final del banco"),
    ),
    (Msg::AddingName, t("AÑADIENDO {name}…")),
    (
        Msg::InputCountHover,
        t("Entradas del show / el máximo que admite un show"),
    ),
    (Msg::AddTileKinds, t("color sólido · barras de color")),
    (Msg::NoThumbnail, t("sin miniatura")),
    // Tally.
    (Msg::TallyProgram, t("PROGRAMA")),
    (Msg::TallyPreview, t("PREVIO")),
    (Msg::TallyProgramDesired, t("PROGRAMA PEDIDO")),
    (Msg::TallyPreviewDesired, t("PREVIO PEDIDO")),
    (Msg::TallyNone, t("SIN TALLY")),
    // Dates and times.
    (Msg::TimeAm, t("a. m.")),
    (Msg::TimePm, t("p. m.")),
    (Msg::DateLong, t("{day} {month} {year}")),
    (Msg::MonthJan, t("ene")),
    (Msg::MonthFeb, t("feb")),
    (Msg::MonthMar, t("mar")),
    (Msg::MonthApr, t("abr")),
    (Msg::MonthMay, t("may")),
    (Msg::MonthJun, t("jun")),
    (Msg::MonthJul, t("jul")),
    (Msg::MonthAug, t("ago")),
    (Msg::MonthSep, t("sept")),
    (Msg::MonthOct, t("oct")),
    (Msg::MonthNov, t("nov")),
    (Msg::MonthDec, t("dic")),
    // Shared console words.
    (Msg::Duration, t("Duración")),
    (Msg::FramesSuffix, t("{frames} fr")),
    (Msg::KeyHint, t("{action}  [{key}]")),
    (Msg::KeyEnter, t("Intro")),
    (Msg::KeyArrows, t("↑/↓")),
    (Msg::WordOn, t("activo")),
    (Msg::WordOff, t("apagado")),
    (Msg::WordYes, t("sí")),
    (Msg::WordNo, t("no")),
    (Msg::WithDefault, t("{value} (predeterminado)")),
    // Transition column.
    (Msg::TransitionSection, t("Transición")),
    (Msg::TBar, t("T-BAR")),
    (Msg::TBarPercent, t("T-BAR {percent}")),
    (Msg::FadeKey, t("FUNDIDO")),
    (Msg::WipeKey, t("CORTINA")),
    (Msg::AlphaKey, t("ALFA")),
    (Msg::SlideKey, t("DESLIZ")),
    (Msg::ZoomKey, t("ZOOM")),
    (Msg::AlphaFadeKind, t("FUNDIDO ALFA")),
    (
        Msg::AutoFadeHover,
        t("Fundir Previo a Programa con la duración compartida"),
    ),
    (
        Msg::AutoWipeHover,
        t("Pasar Previo a Programa con una cortina de la duración compartida"),
    ),
    (
        Msg::AutoAlphaHover,
        t("Fundido alfa de Previo a Programa con la duración compartida"),
    ),
    (
        Msg::AutoSlideHover,
        t("Deslizar Previo a Programa con la duración compartida"),
    ),
    (
        Msg::AutoZoomHover,
        t("Zoom de Previo a Programa con la duración compartida"),
    ),
    (Msg::VerticalWipeKey, t("CORTINA V")),
    (Msg::VerticalSlideKey, t("DESLIZ V")),
    (Msg::FlyKey, t("VUELO")),
    (Msg::FlyRotateKey, t("VUELO GIRO")),
    (Msg::CubeKey, t("CUBO")),
    (Msg::CubeZoomKey, t("CUBO ZOOM")),
    (Msg::CrossZoomKey, t("ZOOM CRUZ")),
    (Msg::MergeKey, t("FUSIÓN")),
    (Msg::StingerFavoriteKey, t("STING {slot}")),
    (Msg::MoreTransitionsKey, t("MÁS")),
    (
        Msg::MoreTransitionsHover,
        t("Lanzar otra transición automática con la duración compartida"),
    ),
    (
        Msg::AutoKindHover,
        t("Lanzar {kind} de Previo a Programa con la duración compartida"),
    ),
    (Msg::FavoritesLabel, t("FAVORITAS")),
    (
        Msg::FavoriteHover,
        t("Favorita {slot}: lanzar {effect} ({frames}); clic derecho para asignar"),
    ),
    (
        Msg::FavoriteCutHover,
        t("Favorita {slot}: corte de Previo a Programa; clic derecho para asignar"),
    ),
    (Msg::AssignFavorite, t("Asignar…")),
    (
        Msg::AssignFavoriteHover,
        t("Asignar la favorita {slot} a una transición con la duración compartida ({frames})"),
    ),
    (
        Msg::AssignFavoriteItemHover,
        t("Hacer que la favorita {slot} lance {effect}"),
    ),
    (
        Msg::DurationHover,
        t("Duración compartida de las transiciones automáticas en fotogramas (arrastre o escriba)"),
    ),
    (Msg::ManualLabel, t("MANUAL")),
    (
        Msg::ManualStartHover,
        t("Iniciar una transición manual reversible: {kind}"),
    ),
    (
        Msg::ReasonManualStartFirst,
        t("no hay ninguna transición manual activa; inicie una con una tecla MANUAL"),
    ),
    (Msg::PositionShort, t("POS")),
    (
        Msg::PositionHover,
        t("Posición exacta de la T-bar de 0 a 10000 puntos básicos; los valores menores revierten"),
    ),
    (Msg::CommitKey, t("COMPLETAR")),
    (
        Msg::CommitHover,
        t("Completar la transición manual activa en su dirección actual"),
    ),
    (Msg::NoManualTransition, t("SIN TRANSICIÓN MANUAL")),
    (Msg::Syncing, t("SINCRONIZANDO")),
    (Msg::ManualStartHint, t("Con {keys}")),
    (Msg::ManualInactive, t("INACTIVA")),
    (
        Msg::ManualStateActive,
        t("{kind} {from}->{to} @ {position} PB"),
    ),
    (
        Msg::ManualStatusHover,
        t("Pedido: {desired}\nRealizado: {realized}"),
    ),
    (
        Msg::ManualUnconfirmed,
        t("El daemon aún no ha confirmado la transición manual pedida."),
    ),
    (
        Msg::TBarHoverActive,
        t(
            "T-bar manual: arrastre la palanca (abajo = inicio, arriba = fin) y luego {commit} o {cancel}  [Esc]",
        ),
    ),
    (
        Msg::TBarHoverIdle,
        t("T-bar manual: inicie una transición manual ({keys}) para habilitar la palanca"),
    ),
    // Fade to black.
    (
        Msg::FtbHover,
        t("Fundir a negro el vídeo y el audio realizados de Programa"),
    ),
    (
        Msg::FtbReverseHover,
        t("Revertir el fundido a negro y restaurar Programa en vivo"),
    ),
    (
        Msg::FtbDurationHover,
        t("Duración del fundido a negro en fotogramas (arrastre o escriba)"),
    ),
    (Msg::FtbStateUnavailable, t("ESTADO FTB NO DISPONIBLE")),
    (Msg::FtbProgramLive, t("PROGRAMA EN VIVO")),
    (Msg::FtbProgramBlack, t("PROGRAMA EN NEGRO")),
    (Msg::FtbFading, t("FUNDIENDO A NEGRO {percent}")),
    (Msg::FtbRestoring, t("RESTAURANDO · {percent} NEGRO")),
    (Msg::FtbBlackRequested, t("{state} · NEGRO PEDIDO")),
    (Msg::FtbLiveRequested, t("{state} · VIVO PEDIDO")),
    (Msg::FtbWordBlack, t("NEGRO")),
    (Msg::FtbTarget, t("OBJETIVO {target} @ {position}")),
    (
        Msg::FtbStateHover,
        t(
            "Estado del fundido a negro al aire informado por el daemon (realizado).\nPEDIDO {desired}\nREALIZADO {realized}",
        ),
    ),
    // Monitors.
    (
        Msg::MonitorNoPreviewTap,
        t("SIN TOMA DE PREVIO · imagen nativa de Previo no generada"),
    ),
    (
        Msg::MonitorNoPreviewTapNote,
        t(
            "El motor nativo solo renderiza Programa; este monitor muestra el estado de enrutamiento.",
        ),
    ),
    (
        Msg::MonitorNoFeed,
        t("SIN SEÑAL DE VÍDEO · esperando la vista codificada"),
    ),
    (
        Msg::MonitorNoFeedNote,
        t("Solo estado de enrutamiento; no ha llegado ningún fotograma codificado de {bus}."),
    ),
    (
        Msg::MonitorSimulated,
        t("RENDER SIMULADO · vista codificada {width}×{height}"),
    ),
    (
        Msg::MonitorSimulatedNote,
        t(
            "Render simulado de la fuente realizada, con las capas de escena y, en Programa, los overlays activos; el fundido a negro no se compone en esta imagen.",
        ),
    ),
    (
        Msg::MonitorEncoded,
        t("VISTA CODIFICADA {width}×{height} · lectura en CPU"),
    ),
    (
        Msg::MonitorEncodedNote,
        t(
            "Salida nativa reducida y leída en la CPU a {width}×{height}; un monitor de baja cadencia, no el vídeo de salida a cadencia completa.",
        ),
    ),
    (
        Msg::MonitorStaleAge,
        t("OBSOLETO · último fotograma codificado de hace {age}"),
    ),
    (
        Msg::MonitorStaleMismatch,
        t("OBSOLETO · el fotograma codificado no coincide con la fuente realizada"),
    ),
    (
        Msg::MonitorStaleNote,
        t(
            "El último fotograma codificado está obsoleto o es anterior al enrutamiento realizado; se atenúa y no prueba la salida actual.",
        ),
    ),
    (Msg::BadgeSimulated, t("SIMULADO")),
    (Msg::BadgeStale, t("OBSOLETO")),
    (Msg::RoutingUnavailable, t("SIN ESTADO DE ENRUTAMIENTO")),
    (Msg::MonitorDesired, t("PEDIDO: {name}")),
    (
        Msg::MonitorRoutingFootnote,
        t("estado de enrutamiento, no prueba del vídeo de salida"),
    ),
    (
        Msg::MonitorRoutingHover,
        t("{bus} pedido: {desired}\n{bus} realizado: {realized}\n{note}"),
    ),
    (Msg::BusPreviewLower, t("previo")),
    (Msg::BusProgramLower, t("programa")),
    // Audio mixer.
    (Msg::MetersUnavailable, t("MEDIDORES NO DISPONIBLES")),
    (
        Msg::MetersUnavailableHover,
        t("No se ha recibido ninguna muestra de medición de audio del daemon"),
    ),
    (
        Msg::ResetClips,
        plural("BORRAR CLIP", "BORRAR {count} CLIPS"),
    ),
    (
        Msg::ResetClipsHover,
        t("Apagar todas las lámparas de clip retenidas"),
    ),
    (Msg::MetersStale, t("MEDIDORES OBSOLETOS · {age}")),
    (
        Msg::MetersStaleHover,
        t(
            "No ha llegado ningún mensaje de medición de audio en más de un segundo; los medidores muestran los últimos niveles atenuados",
        ),
    ),
    (Msg::MeterLegend, t("RMS · RETENCIÓN · CLIP")),
    (
        Msg::MeterLegendHover,
        t(
            "Los segmentos brillantes muestran el RMS y los tenues el último pico por encima; la línea fina marca ese pico; la marca brillante retiene el pico 1,5 s y luego cae 20 dB/s; la lámpara superior se enclava a 0 dBFS hasta que se pulsa. Zonas: verde por debajo de -18 dBFS, ámbar desde -18, rojo desde -6. La escala se amplía hacia 0 dBFS.",
        ),
    ),
    (Msg::NoAudioState, t("Sin estado de audio")),
    (
        Msg::NoAudioStateDetail,
        t("Los canales aparecen cuando el estado del proyecto se sincroniza."),
    ),
    (
        Msg::PeakClippedHover,
        t("Saturado a 0 dBFS; haga clic para apagar la lámpara de clip"),
    ),
    (
        Msg::PeakStaleHover,
        t(
            "Obsoleto: ningún mensaje de medición nuevo en más de un segundo; última retención de pico {level}",
        ),
    ),
    (
        Msg::PeakHoldHover,
        t("Retención de pico: el pico reciente más alto (dBFS)"),
    ),
    (
        Msg::FaderHover,
        t("Fader de ganancia de la entrada: arrastre; doble clic para 0 dB"),
    ),
    (
        Msg::GainEntryHover,
        t("Ganancia de la entrada en dB (arrastre o escriba)"),
    ),
    (
        Msg::BalanceHover,
        t(
            "Balance estéreo: atenúa el lado izquierdo o derecho de una fuente estéreo sin moverla; L100 todo a la izquierda, C centro, R100 todo a la derecha",
        ),
    ),
    (Msg::PanPrefix, t("PANO")),
    (
        Msg::PanHover,
        t(
            "Panorama: sitúa una fuente mono entre los altavoces izquierdo y derecho con sonoridad constante (-3 dB en el centro, +3 dB del todo a un lado); L100 todo a la izquierda, C centro, R100 todo a la derecha; doble clic para centrar",
        ),
    ),
    (Msg::StripLabelHint, t("Etiqueta")),
    (
        Msg::StripLabelHover,
        t(
            "Etiqueta que muestra este canal del mezclador en lugar del nombre de la entrada (hasta 32 bytes); bórrela para volver a mostrar el nombre de la entrada",
        ),
    ),
    (Msg::StripNamePlateHover, t("{label} (entrada {name})")),
    (Msg::StripGroupNone, t("Sin grupo")),
    (
        Msg::StripGroupHover,
        t(
            "Grupo VCA: el fader del grupo se suma a la ganancia de este canal y el silencio del grupo lo calla, sin cambiar el fader propio del canal",
        ),
    ),
    (Msg::StripGroupItemHover, t("Asignar este canal a {group}")),
    (Msg::StripGroupNoneHover, t("Sacar este canal de su grupo")),
    (
        Msg::GroupStripPlateHover,
        t("{group}: un grupo VCA de {count} canales; no lleva audio propio"),
    ),
    (
        Msg::GroupFaderHover,
        t(
            "Fader del grupo: se suma a la ganancia de cada canal miembro; arrastre; doble clic para 0 dB",
        ),
    ),
    (
        Msg::GroupMuteOnHover,
        t("Grupo silenciado: todos sus canales están callados; haga clic para reactivarlos"),
    ),
    (
        Msg::GroupMuteOffHover,
        t("Silenciar todos los canales de este grupo"),
    ),
    (
        Msg::GroupNameHover,
        t("Renombrar este grupo (hasta 32 bytes)"),
    ),
    (Msg::FtbOutputs, t("Salidas {affected}/{total}")),
    (
        Msg::FtbOutputsHover,
        t(
            "Elija qué salidas funde el fundido a negro; las salidas sin marcar siguen en directo mientras las demás se funden",
        ),
    ),
    (
        Msg::FtbOutputFadesHover,
        t("{output} se funde con el fundido a negro; desmárquela para mantenerla en directo"),
    ),
    (
        Msg::FtbOutputLiveHover,
        t("{output} sigue en directo durante el fundido a negro; márquela para fundirla también"),
    ),
    (Msg::FtbOutputsMenu, t("Salidas del fundido a negro")),
    (Msg::DelayPrefix, t("RET")),
    (
        Msg::DelayHover,
        t("Retardo de la entrada en muestras a la frecuencia de 48 kHz del Máster"),
    ),
    (
        Msg::MuteOnHover,
        t("Silenciada: haga clic para reactivar esta entrada"),
    ),
    (Msg::MuteOffHover, t("Silenciar esta entrada")),
    (
        Msg::SoloOnHover,
        t("En solo: haga clic para liberar el solo"),
    ),
    (Msg::SoloOffHover, t("Poner esta entrada en solo")),
    (
        Msg::AfvHover,
        t("El audio sigue al vídeo: abierto solo mientras la entrada está en Programa"),
    ),
    (Msg::MasterStrip, t("MÁSTER")),
    (Msg::ProgramMix, t("MEZCLA PGM")),
    (Msg::MasterTap, t("PRE-STINGER/ FTB")),
    (
        Msg::MasterTapHover,
        t("Los medidores del Máster se toman antes del Stinger y del fundido a negro"),
    ),
    (
        Msg::MeterChannelHover,
        t("Can. {channel}  pico {peak}  RMS {rms}  retención {hold}"),
    ),
    (Msg::MeterClipMark, t("CLIP")),
    (
        Msg::MeterClickToClear,
        t("Haga clic para apagar la lámpara de clip"),
    ),
    (Msg::NoMeterSample, t("Sin muestra de medición")),
    // Overlays.
    (
        Msg::OverlaysIntro,
        t("TOMAR pone la fuente de Previo en el canal; RETIRAR lo libera."),
    ),
    (
        Msg::OverlaysIntroHover,
        t(
            "La última línea de una tarjeta abierta compara el estado pedido con la última confirmación del daemon. La confirmación es estado de control, no prueba del vídeo de salida.",
        ),
    ),
    (Msg::OverlayOnAir, t("AL AIRE")),
    (Msg::OverlayTaking, t("ENTRANDO")),
    (Msg::OverlayClearing, t("SALIENDO")),
    (Msg::TakeKey, t("TOMAR")),
    (Msg::UpdateKey, t("ACTUALIZAR")),
    (Msg::NextKey, t("SIGUIENTE")),
    (Msg::OverlayOffKey, t("RETIRAR")),
    (
        Msg::OverlayOffHover,
        t("Quitar este canal de overlay de Programa"),
    ),
    (
        Msg::ReasonOverlayAlreadyOff,
        t("este canal de overlay ya está apagado"),
    ),
    (
        Msg::OverlayTakeHover,
        t("Tomar la fuente actual de Previo en este canal de overlay"),
    ),
    (
        Msg::ReasonNoPreviewSource,
        t("no hay ninguna fuente seleccionada en Previo"),
    ),
    (Msg::OverlayNoSource, t("sin fuente")),
    (
        Msg::OverlayUpdateHover,
        t("Actualizar este canal de overlay activo con la fuente actual de Previo"),
    ),
    (
        Msg::ReasonOverlayUpdate,
        t("el canal está apagado o ya muestra la fuente de Previo"),
    ),
    (
        Msg::OverlayQueueHover,
        t("Añadir la fuente actual de Previo a la cola de este overlay"),
    ),
    (
        Msg::OverlayNextHover,
        t("Tomar y quitar la primera entrada de la cola de este overlay"),
    ),
    (
        Msg::ReasonOverlayQueueEmpty,
        t("la cola de este overlay está vacía"),
    ),
    (
        Msg::OverlayTransitionHover,
        t(
            "Recorrer la transición de este canal de overlay (corte, fundido y cada movimiento); clic derecho para elegir",
        ),
    ),
    (
        Msg::OverlayTransitionPickHover,
        t("Entrar y salir este canal de overlay con {kind}"),
    ),
    (
        Msg::OverlayPositionHover,
        t("Cambiar la posición predefinida de este canal de overlay"),
    ),
    (
        Msg::OverlayBorderHover,
        t("Cambiar el borde blanco predefinido de este canal de overlay"),
    ),
    (Msg::OverlayOutputs, t("SALIDAS")),
    (
        Msg::OverlayOutputHover,
        t("Incluir este canal de overlay en esta salida"),
    ),
    (
        Msg::ReasonOverlayNotTaken,
        t("el canal de overlay aún no se ha tomado"),
    ),
    (
        Msg::OverlayOutputsNone,
        t("SALIDAS no disponibles: no hay salidas configuradas"),
    ),
    (
        Msg::OverlayOutputsUnsynced,
        t("SALIDAS no disponibles: el estado del proyecto no está sincronizado"),
    ),
    (Msg::PositionFull, t("COMPLETO")),
    (Msg::PositionTopLeft, t("SI")),
    (Msg::PositionTopRight, t("SD")),
    (Msg::PositionBottomLeft, t("II")),
    (Msg::PositionBottomRight, t("ID")),
    (Msg::BorderNone, t("SIN BORDE")),
    (Msg::BorderThin, t("FINO")),
    (Msg::BorderThick, t("GRUESO")),
    // Stingers tab and Configure card.
    (
        Msg::StingersIntro,
        t(
            "Haga clic en una ranura LISTA para lanzarla de Previo a Programa. Haga clic en cualquier otra para configurarla abajo.",
        ),
    ),
    (
        Msg::StingerDurationShared,
        t("compartida con las transiciones automáticas"),
    ),
    (Msg::StingerStateNotSynced, t("Estado sin sincronizar")),
    (Msg::StingerEmptySlot, t("Ranura vacía")),
    (Msg::StingerCardTitle, t("Stinger S{slot} · {state}")),
    (Msg::StingerCardMedia, t("Medio: {name}")),
    (Msg::StingerCardFire, t("Clic: lanzar de Previo a Programa")),
    (
        Msg::StingerCardCannotFire,
        t("No se puede lanzar: {reason}"),
    ),
    (
        Msg::StingerCardConfigure,
        t("Clic: configurar esta ranura abajo"),
    ),
    (Msg::StingerCardNotReady, t("No está lista para lanzarse")),
    (Msg::StingerSlotPick, t("S{slot} · {state}")),
    (
        Msg::StingerSlotPickHover,
        t("Ranura de Stinger a configurar"),
    ),
    (Msg::StingerNoInput, t("sin entrada")),
    (
        Msg::StingerMediaPickHover,
        t("Entrada que reproduce la ranura (por defecto, el medio de la ranura o, si no, Previo)"),
    ),
    (Msg::StingerConfigure, t("Configurar S{slot}")),
    (
        Msg::StingerReadinessHover,
        t("Disponibilidad replicada de la ranura seleccionada"),
    ),
    (
        Msg::StingerLockedHover,
        t(
            "La configuración de ranuras está bloqueada con el proyecto; lanzar Stingers sigue funcionando",
        ),
    ),
    (
        Msg::StingerConfigNotSynced,
        t("Estado de Stingers sin sincronizar"),
    ),
    (Msg::AssignKey, t("ASIGNAR")),
    (Msg::ClearKey, t("VACIAR")),
    (
        Msg::StingerAssignHover,
        t("Asignar un medio a la ranura de Stinger {slot}"),
    ),
    (
        Msg::StingerAssignNamedHover,
        t(
            "Asignar «{name}» como medio de la ranura de Stinger {slot} (se conservan los demás ajustes)",
        ),
    ),
    (Msg::ReasonChooseMedia, t("elija una entrada como medio")),
    (Msg::StingerNotConfigured, t("S{slot} no está configurada")),
    (
        Msg::StingerSummary,
        t(
            "S{slot}: {media} · punto de corte {cut} · precarga {preload} · audio {audio} · alternativa {fallback}",
        ),
    ),
    (Msg::StingerCutPoint, t("Punto de corte")),
    (Msg::StingerPreload, t("Precarga")),
    (Msg::StingerAudio, t("Audio")),
    (Msg::StingerFallback, t("Alternativa")),
    (Msg::StingerAudioMuted, t("silenciado")),
    (Msg::StingerAudioOnly, t("solo stinger")),
    (Msg::StingerAudioMixed, t("mezclado con Programa")),
    (Msg::StingerFallbackCut, t("corte")),
    (Msg::StingerFallbackFade, t("fundido")),
    (Msg::StingerFallbackKeep, t("mantener Programa")),
    // Connection card.
    (Msg::CardSynchronizing, t("Sincronizando")),
    (
        Msg::CardSynchronizingDetail,
        t("Recibiendo el estado actual del proyecto desde freemixd…"),
    ),
    (Msg::CardStarting, t("Iniciando")),
    (
        Msg::CardStartingDetail,
        t("Iniciando el cliente de control de Studio…"),
    ),
    (Msg::CardConnecting, t("Conectando")),
    (
        Msg::CardConnectingDetail,
        t("Abriendo la conexión de control con freemixd…"),
    ),
    (Msg::CardReconnecting, t("Reconectando")),
    (
        Msg::CardReconnectWaiting,
        t("Esperando antes del siguiente intento de conexión…"),
    ),
    (
        Msg::CardReconnectAttempt,
        t("Conexión perdida · intento {attempt} · reintento en {delay}"),
    ),
    (
        Msg::ConnErrRefused,
        t("El daemon rechazó la conexión: puede que aún no esté en marcha"),
    ),
    (
        Msg::ConnErrInterrupted,
        t("La conexión con el daemon se interrumpió"),
    ),
    (Msg::ConnErrTimedOut, t("El daemon dejó de responder")),
    (
        Msg::ConnErrUnreachable,
        t("No se puede llegar a la dirección del daemon"),
    ),
    (
        Msg::ConnErrNetwork,
        t("Un error de red interrumpió la conexión con el daemon"),
    ),
    (Msg::ConnErrClosed, t("El daemon cerró la conexión")),
    (Msg::ConnErrShutdown, t("El daemon se apagó")),
    (Msg::CardDisconnected, t("Desconectado")),
    (
        Msg::CardDisconnectedDetail,
        t(
            "No hay conexión de control con freemixd. Los controles siguen deshabilitados hasta que se reconecte.",
        ),
    ),
    (
        Msg::CardFailedDetail,
        t("La conexión de control falló. Compruebe que freemixd está en ejecución."),
    ),
    (Msg::CardProtocol, t("Protocolo incompatible")),
    (
        Msg::CardProtocolDetail,
        t(
            "freemixd no seleccionó la versión de protocolo de este Studio. Ejecute compilaciones coincidentes de freemixd y freemix-studio.",
        ),
    ),
    (Msg::CardLastError, t("Último error: {error}")),
    (
        Msg::CardControlsDisabled,
        t(
            "Los controles de conmutación, audio y salidas están deshabilitados hasta que la consola esté lista.",
        ),
    ),
    // Add and Remove input dialogs.
    (
        Msg::AddInputIntro,
        t(
            "Añade al final del banco una entrada generada en vivo, con audio en silencio y un canal de mezclador predeterminado. Las entradas de medios, captura y red se añaden sin conexión.",
        ),
    ),
    (Msg::AddInputKind, t("Tipo")),
    (Msg::NameSolidColour, t("Color sólido")),
    (Msg::NameColourBars, t("Barras de color")),
    (Msg::KindSolidKey, t("COLOR SÓLIDO")),
    (Msg::KindBarsKey, t("BARRAS DE COLOR")),
    (
        Msg::KindSolidHover,
        t("Un color sólido a pantalla completa"),
    ),
    (Msg::KindBarsHover, t("Señal de prueba de barras de color")),
    (Msg::AddInputName, t("Nombre")),
    (Msg::NameSuggested, t("Nombre único sugerido")),
    (Msg::NameFree, t("El nombre está libre")),
    (Msg::AddInputPreview, t("Muestra")),
    (
        Msg::AddInputIdPosition,
        t("entrada {input} · posición {position}"),
    ),
    (
        Msg::AddInputNoIdPosition,
        t("sin id libre · posición {position}"),
    ),
    (Msg::AddInputColour, t("Color")),
    (Msg::ColourPickHover, t("Elegir cualquier color")),
    (Msg::ColourBlack, t("Negro")),
    (Msg::ColourWhite, t("Blanco")),
    (Msg::ColourGrey, t("Gris")),
    (Msg::ColourRed, t("Rojo")),
    (Msg::ColourGreen, t("Verde")),
    (Msg::ColourBlue, t("Azul")),
    (Msg::ColourAmber, t("Ámbar")),
    (Msg::ColourMagenta, t("Magenta")),
    (
        Msg::AddInputUndoNote,
        t("Deshacer quita de nuevo una entrada añadida"),
    ),
    (Msg::AddInputKey, t("AÑADIR")),
    (Msg::AddInputKeyHover, t("Añadir esta entrada")),
    (Msg::AddInputCancelHover, t("Cerrar sin añadir")),
    (
        Msg::ReasonNoFreeId,
        t("no queda ningún identificador de entrada libre"),
    ),
    (Msg::PreviewInputTag, t("ENTRADA {input}")),
    (Msg::PreviewSilent, t("SIN AUDIO")),
    (
        Msg::RemoveInputQuestion,
        t("¿Quitar «{name}» (entrada {input}, posición {position})?"),
    ),
    (
        Msg::RemoveInputQuestionShort,
        t("¿Quitar «{name}» (entrada {input})?"),
    ),
    (
        Msg::RemoveInputWarning,
        t(
            "Su canal de mezclador y su candado se van con ella. La eliminación no se puede deshacer: borra el historial de rehacer y olvida los pasos de deshacer que nombran esta entrada.",
        ),
    ),
    (Msg::RemoveKey, t("QUITAR")),
    (Msg::RemoveKeyHover, t("Quitar esta entrada del show")),
    (Msg::RemoveCancelHover, t("Conservar esta entrada")),
    // Side-panel output rows.
    (Msg::SlotsCount, plural("{count} RANURA", "{count} RANURAS")),
    (Msg::RecordingOfTotal, t("{on_air} / {total} GRABANDO")),
    (
        Msg::TargetsCount,
        plural("{count} DESTINO", "{count} DESTINOS"),
    ),
    (Msg::TargetsUnknown, t("— DESTINOS")),
    (Msg::LiveOfTotal, t("{on_air} / {total} EN VIVO")),
    (
        Msg::RecorderRowHover,
        t(
            "Ranura de grabación {slot}: {status}\nMuestra el estado de grabación pedido al daemon; el protocolo no informa de archivo ni duración realizados.",
        ),
    ),
    (
        Msg::StreamsNoneConfigured,
        t("No hay destinos de transmisión configurados"),
    ),
    (
        Msg::StreamsNotSynced,
        t("Estado de transmisiones sin sincronizar"),
    ),
    (
        Msg::StreamRowHover,
        t("{name}\nPedido {desired} · realizado {realized}"),
    ),
    (Msg::StartKey, t("INICIAR")),
    (Msg::StopKey, t("DETENER")),
    (
        Msg::StartStreamTarget,
        t("Iniciar el destino de transmisión «{name}»"),
    ),
    (
        Msg::StopStreamTarget,
        t("Detener el destino de transmisión «{name}»"),
    ),
    (
        Msg::ReasonStreamPending,
        t("un comando anterior para este destino de transmisión sigue pendiente"),
    ),
    // Overlay diagnostic line.
    (
        Msg::OverlayDiagLine,
        t("O{channel} · pedido: {desired} · confirmado: {confirmed}"),
    ),
    (
        Msg::OverlayDiagState,
        t("{source}, {active}, opacidad {opacity}, cola {queued}, {position}, {border}"),
    ),
    (Msg::OverlayDiagUnavailable, t("no disponible")),
    (Msg::OverlayDiagActive, t("activo")),
    (Msg::OverlayDiagInactive, t("inactivo")),
    // Stream transport diagnostics (output row tooltip).
    (
        Msg::StreamDiagTransport,
        t("Transporte: {link} · {bytes} enviados · {queued} en cola · {dropped} descartados"),
    ),
    (Msg::StreamDiagConnected, t("conectado")),
    (Msg::StreamDiagDisconnected, t("sin conexión")),
    (Msg::StreamDiagFailure, t("Fallo: {failure}")),
    (
        Msg::StreamDiagLocal,
        plural(
            "HLS local: {count} segmento de {target} · {free} libres",
            "HLS local: {count} segmentos de {target} · {free} libres",
        ),
    ),
    (
        Msg::StreamDiagNoSample,
        t("Aún no hay muestra de transporte"),
    ),
    // Input fallback name and catalogue refusals.
    (Msg::InputFallbackName, t("Entrada {input}")),
    (Msg::AddRefused, t("Alta rechazada ({code}): {advice}")),
    (Msg::RemoveRefused, t("Baja rechazada ({code}): {advice}")),
    (
        Msg::RefuseDuplicateInput,
        t("La entrada {input} ya existe; abra Añadir entrada de nuevo para obtener un id libre."),
    ),
    (
        Msg::RefuseTooManyInputs,
        t("El show ya tiene el máximo de {limit} entradas; quite una entrada primero."),
    ),
    (
        Msg::RefuseUnknownInput,
        t("«{name}» ya no existe; otro editor la quitó."),
    ),
    (
        Msg::RefuseInvalidName,
        t("Use un nombre no vacío de 128 bytes como máximo."),
    ),
    (
        Msg::RefuseDuplicateName,
        t("Otra entrada ya se llama «{name}»; elija otro nombre."),
    ),
    (
        Msg::RefuseProjectLocked,
        t(
            "El proyecto está bloqueado; abra el candado de la barra superior para cambiar entradas.",
        ),
    ),
    (
        Msg::RefuseInputLocked,
        t("«{name}» tiene su propio candado; ábralo para quitarla."),
    ),
    (
        Msg::RefuseUnavailable,
        t(
            "Este daemon no puede cambiar sus entradas en marcha (medios nativos); añada o quite entradas sin conexión.",
        ),
    ),
    (Msg::InUseOnProgram, t("«{name}» está en Programa.")),
    (Msg::InUseOnPreview, t("«{name}» está en Previo.")),
    (
        Msg::InUseTransition,
        t("«{name}» está en una transición en curso."),
    ),
    (
        Msg::InUseManualTransition,
        t("«{name}» está en la transición manual activa."),
    ),
    (
        Msg::InUseOverlaySource,
        t("«{name}» es la fuente del overlay {channel}."),
    ),
    (
        Msg::InUseOverlayQueue,
        t("«{name}» está en la cola del overlay {channel}."),
    ),
    (
        Msg::InUseStinger,
        t("«{name}» es el medio del Stinger {slot}."),
    ),
    (Msg::InUseOther, t("«{name}» está en uso: {detail}.")),
    (
        Msg::AdviceRouteAway,
        t("Enrute otra entrada a ese bus primero."),
    ),
    (
        Msg::AdviceWaitTransition,
        t("Espere a que termine la transición."),
    ),
    (
        Msg::AdviceOverlay,
        t("Retire el overlay o vacíe su cola primero."),
    ),
    (
        Msg::AdviceStinger,
        t("Vacíe o reasigne esa ranura de Stinger primero."),
    ),
    (
        Msg::AdviceOffline,
        t("Quite esa referencia sin conexión primero."),
    ),
    // Titles tab.
    (
        Msg::TitlesIntro,
        t(
            "Edite los campos de un título, previsualícelos y llévelo al aire. Tomar y Actualizar en vivo emiten exactamente los valores previsualizados.",
        ),
    ),
    (Msg::TitlesNone, t("Sin títulos")),
    (
        Msg::TitlesNoneDetail,
        t("Este show no tiene entradas de título."),
    ),
    (
        Msg::TitlesNotSynced,
        t("Los títulos aparecen cuando el estado del proyecto está sincronizado."),
    ),
    (
        Msg::TitlesWaiting,
        t("Esperando los valores del título desde el daemon"),
    ),
    (Msg::TitleRowHover, t("Editar {name}")),
    (Msg::TitleEdited, t("EDITADO")),
    (Msg::TitleConflict, t("CONFLICTO")),
    (Msg::TitleOff, t("FUERA DEL AIRE")),
    (Msg::TitleOnChannel, t("en O{channel}")),
    (Msg::TitleOnProgram, t("en Programa")),
    (Msg::FieldTypeText, t("TEXTO")),
    (Msg::FieldTypeNumber, t("NÚMERO")),
    (Msg::FieldTypeColor, t("COLOR")),
    (Msg::FieldTypeImage, t("IMAGEN")),
    (Msg::TitleFieldDiscard, t("Descartar")),
    (
        Msg::TitleFieldDiscardHover,
        t("Descartar esta edición y conservar el valor del servidor"),
    ),
    (
        Msg::TitleFieldChanged,
        t("Cambió en el servidor desde que empezó a editar"),
    ),
    (Msg::TitleColorHint, t("#RRGGBB o #RRGGBBAA")),
    (Msg::TitleColorPickHover, t("Elegir un color")),
    (Msg::TitleImageHint, t("URI del recurso")),
    (Msg::TitleMine, t("MÍO")),
    (Msg::TitleTheirs, t("SUYO")),
    (Msg::TitleFieldRemovedValue, t("eliminado del título")),
    (
        Msg::TitleConflictBanner,
        t(
            "Otro operador cambió este título. Compare los valores y luego Rebase para conservar sus ediciones sobre las suyas, o Descarte las propias.",
        ),
    ),
    (Msg::TitleRejectedBanner, t("Rechazado: {reason}")),
    (Msg::TitleRebase, t("Rebasar")),
    (
        Msg::TitleRebaseHover,
        t(
            "Conservar sus ediciones sobre los valores actuales del servidor; previsualice de nuevo antes de emitir",
        ),
    ),
    (Msg::TitleDiscardAll, t("Descartar todo")),
    (
        Msg::TitleDiscardAllHover,
        t("Descartar todas las ediciones de este título y adoptar los valores del servidor"),
    ),
    (Msg::TitlePreviewKey, t("Previsualizar")),
    (
        Msg::TitlePreviewHover,
        t("Preparar Tomar y Actualizar en vivo con exactamente estos valores"),
    ),
    (Msg::TitlePreviewed, t("PREVISUALIZADO")),
    (Msg::TitlePreviewHeading, t("Se emite como se previsualizó")),
    (Msg::TitleFromDraft, t("editado")),
    (Msg::TitleChannel, t("Canal")),
    (
        Msg::TitleChannelHover,
        t("Canal de overlay en el que Tomar pone este título"),
    ),
    (
        Msg::TitleChannelBusy,
        t("O{channel} muestra ahora {name}; Tomar lo reemplaza."),
    ),
    (Msg::TitlePending, t("Esperando al servidor…")),
    (Msg::TitleOpSave, t("Guardar")),
    (Msg::TitleOpTake, t("Tomar")),
    (Msg::TitleOpUpdate, t("Actualizar en vivo")),
    (Msg::TitleOpOffAir, t("Retirar del aire")),
    (
        Msg::TitleOpSaveHover,
        t("Guardar las ediciones sin salir al aire"),
    ),
    (
        Msg::TitleOpTakeHover,
        t("Llevar al aire el título previsualizado en O{channel}"),
    ),
    (
        Msg::TitleOpUpdateHover,
        t("Actualizar el título al aire con las ediciones previsualizadas"),
    ),
    (Msg::TitleOpOffAirHover, t("Retirar el título del aire")),
    (
        Msg::TitleBlockedPending,
        t("un comando de este título espera al servidor"),
    ),
    (
        Msg::TitleBlockedStale,
        t("el título cambió en el servidor; rebase o descarte sus ediciones primero"),
    ),
    (
        Msg::TitleBlockedInvalid,
        t("corrija primero los campos no válidos"),
    ),
    (
        Msg::TitleBlockedNotPreviewed,
        t("previsualice primero las ediciones"),
    ),
    (Msg::TitleBlockedNoChanges, t("no hay ediciones que enviar")),
    (Msg::TitleBlockedOnAir, t("el título está al aire")),
    (Msg::TitleBlockedOffAir, t("el título está fuera del aire")),
    (Msg::TitleRejUnknownTitle, t("el título ya no existe")),
    (Msg::TitleRejUnknownField, t("el título no tiene ese campo")),
    (
        Msg::TitleRejDuplicateField,
        t("un campo se envió dos veces"),
    ),
    (
        Msg::TitleRejTypeMismatch,
        t("un campo tiene el tipo equivocado"),
    ),
    (
        Msg::TitleRejInvalidValue,
        t("el valor de un campo no es válido"),
    ),
    (
        Msg::TitleRejInvalidFields,
        t("el servidor rechazó los valores de los campos"),
    ),
    (
        Msg::TitleRejTooLarge,
        t("el contenido del título es demasiado grande"),
    ),
    (Msg::TitleRejAlreadyOnAir, t("el título ya está al aire")),
    (Msg::TitleRejNotOnAir, t("el título no está al aire")),
    (
        Msg::TitleRejOnProgram,
        t("el título está en Programa; cambie Programa a otra entrada"),
    ),
    (
        Msg::TitleRejMultipleOverlays,
        t("el título está en varios canales de overlay; retire cada uno desde la pestaña Overlays"),
    ),
    (
        Msg::TitleRejLocked,
        t("el proyecto o el título está bloqueado para edición"),
    ),
    (
        Msg::TitleRejNotPermitted,
        t("el rol de esta sesión no lo permite"),
    ),
    (
        Msg::TitleRejRevisionExhausted,
        t("el contador de revisiones del título se agotó"),
    ),
    (Msg::TitleRejOther, t("el servidor rechazó el comando")),
    (
        Msg::FieldErrTextTooLong,
        t("El texto supera los {max} caracteres."),
    ),
    (Msg::FieldErrNumber, t("Introduzca un número entero.")),
    (
        Msg::FieldErrColor,
        t("Introduzca un color como #RRGGBB o #RRGGBBAA."),
    ),
    (
        Msg::FieldErrEmptyImage,
        t("Introduzca un recurso de imagen."),
    ),
    (
        Msg::FieldErrImageTooLong,
        t("La referencia de imagen supera los {max} caracteres."),
    ),
    (
        Msg::FieldErrTypeMismatch,
        t("Este campo admite {expected}, no {actual}."),
    ),
    (Msg::FieldErrRemoved, t("Este campo se eliminó del título.")),
    // Playlist tab.
    (Msg::TabPlaylist, t("Lista")),
    (
        Msg::PlaylistIntro,
        t(
            "Lleva las entradas a Programa en orden, cada una durante su duración y con su transición. La lista solo cambia con la reproducción detenida.",
        ),
    ),
    (Msg::PlaylistNone, t("Sin elementos")),
    (
        Msg::PlaylistNoneDetail,
        t("Añada una entrada abajo para crear la lista."),
    ),
    (
        Msg::PlaylistNotSynced,
        t("La lista aparece cuando el estado del proyecto está sincronizado."),
    ),
    (Msg::PlaylistWaiting, t("Esperando la lista del daemon")),
    (Msg::PlaylistStateArmed, t("PREPARADA")),
    (Msg::PlaylistStatePlaying, t("REPRODUCIENDO")),
    (Msg::PlaylistStatePaused, t("EN PAUSA")),
    (Msg::PlaylistStateFinished, t("TERMINADA")),
    (Msg::PlaylistDisabledBadge, t("OMITIDO")),
    (Msg::PlaylistRemaining, t("Restante")),
    (Msg::PlaylistStartsIn, t("Empieza en")),
    (Msg::PlaylistElapsedOf, t("de {duration}")),
    (Msg::PlaylistNextName, t("Siguiente: {name}")),
    (Msg::PlaylistNextEnd, t("Siguiente: fin de la lista")),
    (Msg::PlaylistNextRepeat, t("Siguiente: repite {name}")),
    (
        Msg::PlaylistEntryCount,
        plural("{count} elemento", "{count} elementos"),
    ),
    (Msg::PlaylistLoop, t("Repetir")),
    (Msg::PlaylistLoopOff, t("No")),
    (Msg::PlaylistLoopAll, t("Todo")),
    (Msg::PlaylistLoopOne, t("Uno")),
    (
        Msg::PlaylistLoopOffHover,
        t("Detenerse tras el último elemento activo"),
    ),
    (
        Msg::PlaylistLoopAllHover,
        t("Volver a empezar por el primer elemento activo tras el último"),
    ),
    (Msg::PlaylistLoopOneHover, t("Repetir el elemento al aire")),
    (Msg::PlaylistStart, t("Iniciar")),
    (
        Msg::PlaylistStartHover,
        t("Iniciar la lista ahora desde su primer elemento activo"),
    ),
    (Msg::PlaylistStop, t("Detener")),
    (
        Msg::PlaylistStopHover,
        t("Detener la lista; Programa mantiene su fuente actual"),
    ),
    (Msg::PlaylistPause, t("Pausa")),
    (
        Msg::PlaylistPauseHover,
        t("Retener el elemento al aire; su tiempo restante se detiene"),
    ),
    (Msg::PlaylistResume, t("Reanudar")),
    (
        Msg::PlaylistResumeHover,
        t("Continuar el elemento al aire desde donde se pausó"),
    ),
    (Msg::PlaylistPrevious, t("Anterior")),
    (
        Msg::PlaylistPreviousHover,
        t("Llevar ahora al aire el elemento activo anterior"),
    ),
    (Msg::PlaylistNext, t("Siguiente")),
    (
        Msg::PlaylistNextHover,
        t("Llevar ahora al aire el siguiente elemento activo"),
    ),
    (Msg::PlaylistJumpHover, t("Llevar {name} al aire ahora")),
    (
        Msg::PlaylistStartFromHover,
        t("Iniciar la lista desde {name}"),
    ),
    (Msg::PlaylistMoveUpHover, t("Mover este elemento antes")),
    (Msg::PlaylistMoveDownHover, t("Mover este elemento después")),
    (
        Msg::PlaylistRemoveHover,
        t("Quitar este elemento de la lista"),
    ),
    (
        Msg::PlaylistEnableHover,
        t("Incluir este elemento cuando la lista se reproduce"),
    ),
    (
        Msg::PlaylistDisableHover,
        t("Saltar este elemento cuando la lista se reproduce"),
    ),
    (
        Msg::PlaylistRowHover,
        t("{position}. {name} · {transition} · {duration}"),
    ),
    (Msg::PlaylistTransitionFrames, t("{transition} {frames} fr")),
    (Msg::PlaylistEditorNew, t("Nuevo elemento")),
    (Msg::PlaylistEditorEdit, t("Elemento {position}")),
    (Msg::PlaylistInput, t("Entrada")),
    (Msg::PlaylistInputPick, t("Elija una entrada")),
    (
        Msg::PlaylistDurationHint,
        t("HH:MM:SS:FF, MM:SS o segundos"),
    ),
    (
        Msg::PlaylistDurationHover,
        t("HH:MM:SS:FF, MM:SS o segundos. Intro añade el elemento o actualiza el seleccionado"),
    ),
    (Msg::PlaylistFramesLabel, t("Fotogramas")),
    (Msg::PlaylistAdd, t("Añadir")),
    (
        Msg::PlaylistAddHover,
        t("Añadir este elemento al final de la lista"),
    ),
    (Msg::PlaylistUpdate, t("Actualizar")),
    (
        Msg::PlaylistUpdateHover,
        t("Sustituir el elemento {position} por estos valores"),
    ),
    (Msg::PlaylistNewEntry, t("Nuevo")),
    (
        Msg::PlaylistNewEntryHover,
        t("Vaciar el formulario para añadir un elemento nuevo"),
    ),
    (
        Msg::PlaylistErrDuration,
        t("Introduzca una duración de un fotograma a {max}."),
    ),
    (
        Msg::PlaylistReasonRunning,
        t("la lista se está reproduciendo; deténgala para cambiarla"),
    ),
    (
        Msg::PlaylistReasonNotOnAir,
        t("ningún elemento de la lista está al aire"),
    ),
    (
        Msg::PlaylistReasonActive,
        t("la lista ya se está reproduciendo"),
    ),
    (
        Msg::PlaylistReasonNoEnabled,
        t("la lista no tiene elementos activos"),
    ),
    (
        Msg::PlaylistReasonNoNext,
        t("no hay un siguiente elemento activo"),
    ),
    (
        Msg::PlaylistReasonNoPrevious,
        t("no hay un elemento activo anterior"),
    ),
    (
        Msg::PlaylistReasonEntryDisabled,
        t("este elemento está inactivo"),
    ),
    (Msg::PlaylistReasonOnAir, t("este elemento ya está al aire")),
    (
        Msg::PlaylistReasonFull,
        t("la lista ya tiene el máximo de elementos"),
    ),
    (
        Msg::PlaylistReasonInvalid,
        t("corrija primero la duración del elemento"),
    ),
    (Msg::PlaylistReasonNoInput, t("elija primero una entrada")),
    (
        Msg::PlaylistReasonNoChange,
        t("el elemento ya tiene estos valores"),
    ),
    (
        Msg::PlaylistReasonFirst,
        t("este elemento ya es el primero"),
    ),
    (Msg::PlaylistReasonLast, t("este elemento ya es el último")),
    // Hover help for every control (RC-018).
    (
        Msg::MenuFileHover,
        t("Preferencias y salir; el daemon guarda el show tras cada edición"),
    ),
    (
        Msg::MenuEditHover,
        t(
            "Deshacer, rehacer, el candado de edición y el nombre y la posición de la entrada en Previo",
        ),
    ),
    (
        Msg::MenuViewHover,
        t("Paneles, la pestaña del panel lateral, el diseño, el idioma y el reloj"),
    ),
    (
        Msg::MenuInputsHover,
        t("Añadir, quitar, seleccionar y cortar entradas"),
    ),
    (
        Msg::MenuOutputHover,
        t("Transiciones, fundido a negro, la T-bar, grabación, transmisiones y Stingers"),
    ),
    (
        Msg::MenuHelpHover,
        t("Atajos de teclado e información de la versión"),
    ),
    (Msg::RenamePreviewHover, t("Renombrar la entrada en Previo")),
    (
        Msg::MoveEarlierHover,
        t("Mover la entrada en Previo un lugar antes en el banco"),
    ),
    (
        Msg::MoveLaterHover,
        t("Mover la entrada en Previo un lugar después en el banco"),
    ),
    (
        Msg::TabOverlaysHover,
        t(
            "Canales de overlay O1–O8: tomar, encolar y dar estilo a fuentes superpuestas sobre Programa",
        ),
    ),
    (
        Msg::TabStingersHover,
        t("Ranuras de Stinger S1–S8: lanzar una ranura configurada o configurar su medio"),
    ),
    (
        Msg::TabTitlesHover,
        t("Entradas de título: editar sus campos, previsualizar y llevarlas al aire"),
    ),
    (
        Msg::TabPlaylistHover,
        t("Un orden automático de entradas con duraciones y transiciones"),
    ),
    (
        Msg::SelectNamedOnPreviewHover,
        t("Seleccionar «{name}» en Previo"),
    ),
    (
        Msg::CutNamedToProgramHover,
        t("Seleccionar «{name}» en Previo y cortarla a Programa"),
    ),
    (
        Msg::NoStreamTargetsHover,
        t("Iniciar y detener los destinos de transmisión del show"),
    ),
    (
        Msg::ClearStingerMenuHover,
        t("Vaciar una ranura de Stinger configurada"),
    ),
    (
        Msg::AboutHover,
        t("Versión, licencia y qué es FreeMix Studio"),
    ),
    (
        Msg::TileRenameHover,
        t("Dar un nuevo nombre a esta entrada"),
    ),
    (
        Msg::TileMoveLeftHover,
        t("Mover esta entrada un lugar antes en el banco"),
    ),
    (
        Msg::TileMoveRightHover,
        t("Mover esta entrada un lugar después en el banco"),
    ),
    (
        Msg::RenameFieldHover,
        t("El nuevo nombre de la entrada; Intro lo aplica"),
    ),
    (
        Msg::StingerSlotOptionHover,
        t("Configurar la ranura de Stinger S{slot}"),
    ),
    (
        Msg::StingerMediaOptionHover,
        t("Reproducir «{name}» como medio de esta ranura"),
    ),
    (
        Msg::TitleTextFieldHover,
        t("Texto que el título muestra en el campo"),
    ),
    (
        Msg::TitleNumberFieldHover,
        t("Número entero que el título muestra en el campo"),
    ),
    (
        Msg::TitleColorFieldHover,
        t("Color del campo en hexadecimal #RRGGBB o #RRGGBBAA"),
    ),
    (
        Msg::TitleImageFieldHover,
        t("URI del recurso de imagen que el título muestra en el campo"),
    ),
    (
        Msg::TitleChannelOptionHover,
        t("Tomar este título en el canal de overlay O{channel}"),
    ),
    (
        Msg::PlaylistInputPickHover,
        t("Entrada que este elemento pone al aire"),
    ),
    (
        Msg::PlaylistInputOptionHover,
        t("Poner «{name}» al aire en este elemento"),
    ),
    (
        Msg::PlaylistTransitionPickHover,
        t("Transición que lleva este elemento al aire"),
    ),
    (
        Msg::PlaylistTransitionOptionHover,
        t("Llevar este elemento al aire con: {kind}"),
    ),
    (
        Msg::PlaylistTransitionFramesHover,
        t("Duración de la transición en fotogramas (arrastre o escriba)"),
    ),
    (
        Msg::PlaylistReasonCutHasNoLength,
        t("un Corte es instantáneo y no tiene duración"),
    ),
    (
        Msg::AddInputNameHover,
        t("El nombre de la nueva entrada, único en el show; Intro la añade"),
    ),
    (Msg::ColourSwatchHover, t("Usar {colour}  {hex}")),
    (Msg::ViewDetails, t("Controles avanzados")),
    (
        Msg::ViewMixerDetails,
        t("Valores de ganancia, balance y retardo del mezclador"),
    ),
    (
        Msg::ViewMixerDetailsHover,
        t(
            "Mostrar los valores de ganancia, balance y retardo bajo cada canal; el fader y las teclas M, S y AFV se muestran siempre",
        ),
    ),
    (Msg::ViewManualDetails, t("Detalles de la T-bar manual")),
    (
        Msg::ViewManualDetailsHover,
        t(
            "Mostrar la posición exacta de la T-bar, Completar, Cancelar y la tarjeta de estado aunque no haya una transición manual activa",
        ),
    ),
    (Msg::MixerDetailsKey, t("GAN · BAL · RET")),
    (Msg::DetailsKey, t("DETALLES")),
    (
        Msg::DisclosureShowMixer,
        t("Mostrar los valores de ganancia, balance y retardo en cada canal"),
    ),
    (
        Msg::DisclosureHideMixer,
        t("Ocultar los valores; el fader sigue ajustando la ganancia"),
    ),
    (
        Msg::DisclosureShowManual,
        t(
            "Mostrar la posición exacta, Completar, Cancelar y el estado antes de iniciar una transición manual",
        ),
    ),
    (
        Msg::DisclosureHideManual,
        t(
            "Ocultarlos hasta que empiece una transición manual; se muestran mientras haya una activa",
        ),
    ),
    // Animation timeline (GX-011). The keyframe is a "clave" (short for
    // "fotograma clave"); the phases read ENTRADA, EN AIRE, and SALIDA.
    (Msg::AnimSection, t("Animación")),
    (
        Msg::AnimSectionHover,
        t(
            "Animación por fotogramas clave de este gráfico. ENTRADA se reproduce al sacarlo al aire y SALIDA al retirarlo. Haga clic dentro de la línea de tiempo para usar sus atajos de teclado.",
        ),
    ),
    (Msg::AnimPhaseIn, t("ENTRADA")),
    (Msg::AnimPhaseOut, t("SALIDA")),
    (Msg::AnimPhaseHold, t("EN AIRE")),
    (
        Msg::AnimPhaseInHover,
        t(
            "ENTRADA: la animación que se reproduce al sacar el gráfico al aire. Su último fotograma se mantiene mientras el gráfico siga al aire.",
        ),
    ),
    (
        Msg::AnimPhaseOutHover,
        t("SALIDA: la animación que se reproduce al retirar el gráfico del aire."),
    ),
    (
        Msg::AnimPhaseTabClick,
        t("Haga clic para editar esta fase."),
    ),
    (
        Msg::AnimHoldHover,
        t(
            "EN AIRE: mientras está al aire, el gráfico se queda en el último fotograma de ENTRADA. Aquí no hay nada que animar; anime ENTRADA y SALIDA.",
        ),
    ),
    (Msg::AnimLength, t("Duración")),
    (
        Msg::AnimLengthHover,
        t(
            "Duración de esta fase en fotogramas. Al cambiarla, todas las claves de la fase se estiran en proporción (un solo paso de deshacer), igual que al arrastrar el final de la fase en la regla. Arrastre el tirador de la regla con Alt pulsado para cambiar la duración sin mover claves.",
        ),
    ),
    (Msg::AnimFit, t("Ajustar")),
    (
        Msg::AnimFitHover,
        t(
            "Ajustar el zoom para que quepa la fase entera. Ctrl+rueda amplía alrededor del puntero; Mayús+rueda (o un deslizamiento lateral) desplaza en el tiempo.",
        ),
    ),
    (
        Msg::AnimPlayHover,
        t("Reproducir esta fase desde el cabezal"),
    ),
    (Msg::AnimPauseHover, t("Pausar la reproducción")),
    (
        Msg::AnimLoopOnHover,
        t(
            "Repetición activada: la reproducción vuelve al inicio de la fase. Haga clic para detenerse al final",
        ),
    ),
    (
        Msg::AnimLoopOffHover,
        t(
            "Repetición desactivada: la reproducción se detiene al final de la fase. Haga clic para repetir",
        ),
    ),
    (
        Msg::AnimStepBackHover,
        t("Retroceder un fotograma (Mayús: diez fotogramas)"),
    ),
    (
        Msg::AnimStepForwardHover,
        t("Avanzar un fotograma (Mayús: diez fotogramas)"),
    ),
    (Msg::AnimKeyButton, t("◆ Clave")),
    (
        Msg::AnimKeyHover,
        t("Añadir una clave en el cabezal en la fila de propiedad activa, con su valor actual"),
    ),
    (Msg::AnimDelete, t("Eliminar")),
    (Msg::AnimDeleteHover, t("Eliminar las claves seleccionadas")),
    (Msg::AnimCopy, t("Copiar")),
    (
        Msg::AnimCopyHover,
        t("Copiar las claves seleccionadas (tiempos, valores y suavizado)"),
    ),
    (Msg::AnimPaste, t("Pegar")),
    (
        Msg::AnimPasteHover,
        t("Pegar las claves copiadas con la primera en el cabezal"),
    ),
    (
        Msg::AnimUndoHover,
        t("Deshacer la última edición de la animación (un arrastre entero es un solo paso)"),
    ),
    (
        Msg::AnimRedoHover,
        t("Rehacer la última edición de la animación que se deshizo"),
    ),
    (Msg::AnimAdvanced, t("Avanzado")),
    (Msg::AnimSimple, t("Simple")),
    (
        Msg::AnimToAdvancedHover,
        t(
            "Pasar al modo Avanzado: una fila por propiedad con claves individuales, suavizado y un gráfico de valores",
        ),
    ),
    (
        Msg::AnimToSimpleHover,
        t(
            "Pasar al modo Simple: una barra por elemento; estírela para cambiar su tiempo y elija preajustes listos para usar",
        ),
    ),
    (
        Msg::AnimTimecodeHover,
        t(
            "Cabezal / duración de la fase en HH:MM:SS:FF (fotograma {frame} de {total}). Arrastre en la regla para recorrerla.",
        ),
    ),
    (Msg::AnimKeySpace, t("Espacio")),
    (
        Msg::AnimKeyStepBack,
        t("Re Pág, o ← sin claves seleccionadas"),
    ),
    (
        Msg::AnimKeyStepForward,
        t("Av Pág, o → sin claves seleccionadas"),
    ),
    (Msg::AnimKeyDelete, t("Supr")),
    (Msg::AnimKeyRedo, t("Ctrl+Mayús+Z")),
    (
        Msg::AnimReasonDrag,
        t("termine primero el arrastre en curso"),
    ),
    (
        Msg::AnimReasonNoLength,
        t(
            "esta fase aún no tiene duración; aplique un preajuste o arrastre el final de la fase en la regla",
        ),
    ),
    (
        Msg::AnimReasonNoSelection,
        t(
            "no hay claves seleccionadas; haga clic en una clave o arrastre un recuadro alrededor de varias",
        ),
    ),
    (
        Msg::AnimReasonSimpleKey,
        t(
            "las claves se fijan por propiedad en el modo Avanzado; en el modo Simple, elija un preajuste",
        ),
    ),
    (
        Msg::AnimReasonNoRow,
        t("haga clic en una fila de propiedad (o en una de sus claves) para elegir qué animar"),
    ),
    (
        Msg::AnimReasonFirstFrame,
        t("el cabezal está en el primer fotograma"),
    ),
    (
        Msg::AnimReasonLastFrame,
        t("el cabezal está en el último fotograma"),
    ),
    (
        Msg::AnimReasonNothingCopied,
        t("aún no se ha copiado nada; seleccione claves y cópielas primero"),
    ),
    (
        Msg::AnimReasonNothingToUndo,
        t("no hay ninguna edición de la animación que deshacer"),
    ),
    (
        Msg::AnimReasonNothingToRedo,
        t("no hay ninguna edición de la animación que rehacer"),
    ),
    (
        Msg::AnimReasonNoValueRow,
        t("haga clic en una fila de propiedad (o en una de sus claves) para elegir qué editar"),
    ),
    (
        Msg::AnimReasonAlreadyAnimated,
        t("esta propiedad ya está animada en esta fase"),
    ),
    (
        Msg::AnimReasonNoTargets,
        t("el diseño no tiene elementos que animar"),
    ),
    (
        Msg::AnimReasonUnknownTarget,
        t("este elemento ya no está en el diseño"),
    ),
    (
        Msg::AnimRefusedHover,
        t("La edición se rechazó y no cambió nada. Id. del mensaje: {key}"),
    ),
    (
        Msg::AnimHintHover,
        t("Por qué el último atajo no hizo nada"),
    ),
    (Msg::AnimDismissHover, t("Descartar este mensaje")),
    (
        Msg::AnimErrRetimeCollision,
        t("Demasiado corto: dos claves de una propiedad caerían en el mismo fotograma"),
    ),
    (
        Msg::AnimErrKeyCollision,
        t("Ya hay otra clave en ese fotograma"),
    ),
    (
        Msg::AnimErrLimitExceeded,
        t("Eso superaría el límite de tamaño de la animación"),
    ),
    (
        Msg::AnimErrKeyOutsidePhase,
        t("Una clave quedaría después del final de la fase"),
    ),
    (
        Msg::AnimErrInvalidValue,
        t("Ese valor está fuera del rango de la propiedad"),
    ),
    (Msg::AnimErrUnknownKey, t("Esa clave ya no existe")),
    (
        Msg::AnimErrUnknownTarget,
        t("Ese elemento ya no está en el diseño"),
    ),
    (
        Msg::AnimErrZeroSpan,
        t(
            "Todas las claves seleccionadas están en el mismo fotograma, así que no hay nada que estirar",
        ),
    ),
    (
        Msg::AnimErrEmptySelection,
        t("Seleccione primero una o más claves"),
    ),
    (
        Msg::AnimErrEmptyClipboard,
        t("Aún no hay nada copiado: copie claves primero"),
    ),
    (Msg::AnimErrNoDrag, t("No hay ningún arrastre en curso")),
    (
        Msg::AnimErrDragActive,
        t("Termine primero el arrastre en curso"),
    ),
    (Msg::AnimErrNothingToUndo, t("Nada que deshacer")),
    (Msg::AnimErrNothingToRedo, t("Nada que rehacer")),
    (
        Msg::AnimErrInvalidClip,
        t("La animación es incoherente; vuelva a cargarla"),
    ),
    (Msg::AnimErrRefused, t("Se rechazó la edición")),
    (Msg::AnimErrSentence, t("{message}.")),
    (Msg::AnimErrAtFrame, t("{message} (fotograma {frame}).")),
    (
        Msg::AnimErrShortestFits,
        t("{message}. Duración mínima que cabe: {length}."),
    ),
    (
        Msg::AnimErrMaxPhaseFrames,
        t("{message}: como máximo {max} fotogramas por fase."),
    ),
    (
        Msg::AnimErrMaxTracks,
        t("{message}: como máximo {max} propiedades animadas."),
    ),
    (
        Msg::AnimErrMaxTrackKeys,
        t("{message}: como máximo {max} claves por propiedad."),
    ),
    (
        Msg::AnimErrMaxClipKeys,
        t("{message}: como máximo {max} claves en total."),
    ),
    (Msg::AnimPropPositionX, t("Posición X")),
    (Msg::AnimPropPositionY, t("Posición Y")),
    (Msg::AnimPropScaleX, t("Escala X")),
    (Msg::AnimPropScaleY, t("Escala Y")),
    (Msg::AnimPropRotation, t("Rotación")),
    (Msg::AnimPropOpacity, t("Opacidad")),
    (Msg::AnimPropCropLeft, t("Recorte izquierdo")),
    (Msg::AnimPropCropTop, t("Recorte superior")),
    (Msg::AnimPropCropRight, t("Recorte derecho")),
    (Msg::AnimPropCropBottom, t("Recorte inferior")),
    (
        Msg::AnimPropPositionXHover,
        t("Desplazamiento horizontal en píxeles, sumado a la posición del elemento"),
    ),
    (
        Msg::AnimPropPositionYHover,
        t("Desplazamiento vertical en píxeles, sumado a la posición del elemento"),
    ),
    (
        Msg::AnimPropScaleXHover,
        t("Escala de anchura; 100 % es el tamaño diseñado"),
    ),
    (
        Msg::AnimPropScaleYHover,
        t("Escala de altura; 100 % es el tamaño diseñado"),
    ),
    (
        Msg::AnimPropRotationHover,
        t("Rotación en grados, sumada a la rotación diseñada"),
    ),
    (
        Msg::AnimPropOpacityHover,
        t("Opacidad que multiplica la opacidad diseñada; 100 % la deja igual"),
    ),
    (
        Msg::AnimPropCropLeftHover,
        t("Píxeles recortados del borde izquierdo"),
    ),
    (
        Msg::AnimPropCropTopHover,
        t("Píxeles recortados del borde superior"),
    ),
    (
        Msg::AnimPropCropRightHover,
        t("Píxeles recortados del borde derecho"),
    ),
    (
        Msg::AnimPropCropBottomHover,
        t("Píxeles recortados del borde inferior"),
    ),
    (
        Msg::AnimPropColourHover,
        t("Color que sustituye al color diseñado"),
    ),
    (
        Msg::AnimPresetSlideFromLeft,
        t("Deslizar desde la izquierda"),
    ),
    (
        Msg::AnimPresetSlideFromRight,
        t("Deslizar desde la derecha"),
    ),
    (Msg::AnimPresetSlideFromTop, t("Deslizar desde arriba")),
    (Msg::AnimPresetSlideFromBottom, t("Deslizar desde abajo")),
    (Msg::AnimPresetPop, t("Emerger")),
    (Msg::AnimPresetDrop, t("Caída")),
    (
        Msg::AnimPresetFadeHover,
        t("Hace aparecer (o desaparecer) el elemento con un fundido de opacidad"),
    ),
    (
        Msg::AnimPresetSlideFromLeftHover,
        t("Desliza el elemento desde fuera del borde izquierdo"),
    ),
    (
        Msg::AnimPresetSlideFromRightHover,
        t("Desliza el elemento desde fuera del borde derecho"),
    ),
    (
        Msg::AnimPresetSlideFromTopHover,
        t("Desliza el elemento desde encima del cuadro"),
    ),
    (
        Msg::AnimPresetSlideFromBottomHover,
        t("Desliza el elemento desde debajo del cuadro"),
    ),
    (
        Msg::AnimPresetPopHover,
        t("Amplía el elemento desde cero con un pequeño rebase"),
    ),
    (
        Msg::AnimPresetWipeHover,
        t("Descubre el elemento de izquierda a derecha animando su recorte"),
    ),
    (
        Msg::AnimPresetDropHover,
        t("Deja caer el elemento desde arriba con un rebote"),
    ),
    (Msg::AnimEaseLinear, t("Lineal")),
    (Msg::AnimEaseHold, t("Mantener")),
    (Msg::AnimEaseGentleIn, t("Aceleración suave")),
    (Msg::AnimEaseGentleOut, t("Frenada suave")),
    (Msg::AnimEaseGentleInOut, t("Acelerar y frenar suave")),
    (Msg::AnimEaseStrongIn, t("Aceleración fuerte")),
    (Msg::AnimEaseStrongOut, t("Frenada fuerte")),
    (Msg::AnimEaseStrongInOut, t("Acelerar y frenar fuerte")),
    (Msg::AnimEaseSoftIn, t("Aceleración leve")),
    (Msg::AnimEaseSoftOut, t("Frenada leve")),
    (Msg::AnimEaseSoftInOut, t("Acelerar y frenar leve")),
    (Msg::AnimEaseAnticipate, t("Anticipación")),
    (Msg::AnimEaseOvershoot, t("Rebase")),
    (Msg::AnimEaseAnticipateOvershoot, t("Anticipación + rebase")),
    (Msg::AnimEaseBounceIn, t("Rebote al inicio")),
    (Msg::AnimEaseBounceOut, t("Rebote al final")),
    (Msg::AnimEaseBounceInOut, t("Rebote en ambos extremos")),
    (Msg::AnimEaseStep, t("Escalón")),
    (Msg::AnimEaseCustom, t("Curva personalizada")),
    (Msg::AnimEaseMixed, t("Mixto")),
    (
        Msg::AnimEaseLinearHover,
        t("Velocidad constante desde esta clave hasta la siguiente"),
    ),
    (
        Msg::AnimEaseHoldHover,
        t("Mantiene este valor y salta al de la siguiente clave en su fotograma"),
    ),
    (
        Msg::AnimEaseInHover,
        t("Empieza despacio y acelera hacia la siguiente clave"),
    ),
    (
        Msg::AnimEaseOutHover,
        t("Empieza rápido y se asienta suavemente en la siguiente clave (la entrada más natural)"),
    ),
    (
        Msg::AnimEaseInOutHover,
        t("Acelera y luego frena: suave en ambas claves"),
    ),
    (
        Msg::AnimEaseAnticipateHover,
        t("Retrocede un poco antes de avanzar hacia la siguiente clave"),
    ),
    (
        Msg::AnimEaseOvershootHover,
        t("Sobrepasa un poco el valor de la siguiente clave y luego vuelve"),
    ),
    (
        Msg::AnimEaseAnticipateOvershootHover,
        t("Retrocede al principio y se pasa al final"),
    ),
    (
        Msg::AnimEaseBounceInHover,
        t("Rebota unas cuantas veces antes de salir"),
    ),
    (
        Msg::AnimEaseBounceOutHover,
        t("Llega al valor de la siguiente clave con unos rebotes"),
    ),
    (Msg::AnimEaseBounceInOutHover, t("Rebota en ambos extremos")),
    (Msg::AnimEasing, t("Suavizado")),
    (
        Msg::AnimEasingHover,
        t(
            "Cómo avanza el movimiento desde cada clave seleccionada hasta la siguiente. Elija una curva para aplicarla a todas las claves seleccionadas (un solo paso de deshacer).",
        ),
    ),
    (
        Msg::AnimEasingShapeHover,
        t("{easing}: la forma del valor en el tiempo entre dos claves"),
    ),
    (Msg::AnimValue, t("Valor")),
    (
        Msg::AnimValueNoRowHover,
        t("El valor de la fila activa en el cabezal"),
    ),
    (
        Msg::AnimColourAtPlayheadHover,
        t(
            "Color en el cabezal: {value}. Las claves de color se editan en el diseñador de títulos.",
        ),
    ),
    (
        Msg::AnimValueHover,
        t(
            "{property} en el cabezal. Al cambiar el valor se añade una clave aquí o se actualiza la que ya hay (un solo paso de deshacer al soltar).",
        ),
    ),
    (Msg::AnimAnimateProperty, t("+ Animar propiedad")),
    (
        Msg::AnimAnimatePropertyHover,
        t(
            "Empezar a animar otra propiedad: añade una clave en el cabezal con su valor actual y la convierte en la fila activa.",
        ),
    ),
    (
        Msg::AnimAnimateTargetHover,
        t("Animar una propiedad de {name}"),
    ),
    (
        Msg::AnimGraphEmpty,
        t("Gráfico de valores: haga clic en una fila de propiedad para trazarla aquí"),
    ),
    (
        Msg::AnimGraphEmptyHover,
        t(
            "El gráfico de valores traza la propiedad activa a lo largo de esta fase, alineado con la línea de tiempo. Haga clic arriba en una fila de propiedad para elegirla.",
        ),
    ),
    (
        Msg::AnimGraphHover,
        t(
            "Gráfico de valores de {property}: su valor a lo largo de esta fase, alineado fotograma a fotograma con la línea de tiempo. Los puntos son claves; la curva entre ellas sigue el suavizado de cada clave.",
        ),
    ),
    (Msg::AnimGraphReadout, t("fotograma {frame}: {value}")),
    (
        Msg::AnimErrNoSegment,
        t(
            "La última clave de una propiedad no tiene clave siguiente, así que no tiene curva que modelar",
        ),
    ),
    (Msg::AnimEaseCustomEllipsis, t("Curva personalizada…")),
    (
        Msg::AnimEaseCustomHover,
        t(
            "Dé forma a su propia curva con dos tiradores, partiendo de el suavizado actual (un paso de deshacer)",
        ),
    ),
    (
        Msg::AnimCurveEditorHover,
        t(
            "Curva personalizada: el tiempo avanza de izquierda a derecha y el valor de abajo arriba. Arrastre un tirador para cambiar su forma (un paso de deshacer por arrastre); los tiradores se ajustan a cuartos y tercios, mantenga Mayús para moverlos libremente. Por encima del borde superior o por debajo del inferior, el movimiento rebasa o se anticipa.",
        ),
    ),
    (
        Msg::AnimCurveHandleOutHover,
        t(
            "Tirador de salida ({x}, {y}): cómo sale el movimiento de la clave. Más a la derecha llega más lejos; más alto arranca más rápido. Mayús: sin ajuste",
        ),
    ),
    (
        Msg::AnimCurveHandleInHover,
        t(
            "Tirador de llegada ({x}, {y}): cómo llega el movimiento a la clave siguiente. Más a la izquierda llega más lejos; más bajo llega más rápido. Mayús: sin ajuste",
        ),
    ),
    (Msg::AnimCurveStartFrom, t("Partir de")),
    (Msg::AnimCurvePresetEaseIn, t("Entrada suave")),
    (Msg::AnimCurvePresetEaseOut, t("Salida suave")),
    (Msg::AnimCurvePresetEaseInOut, t("Entrada y salida suaves")),
    (Msg::AnimCurvePresetBack, t("Rebase")),
    (
        Msg::AnimCurvePresetHover,
        t(
            "{help}. Sustituye la curva personalizada por esta forma de partida (un paso de deshacer)",
        ),
    ),
    (
        Msg::AnimCurveX1Hover,
        t(
            "x1, influencia de salida: hasta dónde llega el tirador de salida a lo largo del tramo (de 0 a 1). Escriba o arrastre; se aplica al soltar (un paso de deshacer)",
        ),
    ),
    (
        Msg::AnimCurveY1Hover,
        t(
            "y1, altura de salida: el valor del tirador de salida (de -1 a 2); por encima de 1 rebasa. Se aplica al soltar (un paso de deshacer)",
        ),
    ),
    (
        Msg::AnimCurveX2Hover,
        t(
            "x2, influencia de llegada: dónde se sitúa el tirador de llegada a lo largo del tramo (de 0 a 1); cuanto menor, más lejos llega. Se aplica al soltar (un paso de deshacer)",
        ),
    ),
    (
        Msg::AnimCurveY2Hover,
        t(
            "y2, altura de llegada: el valor del tirador de llegada (de -1 a 2); por debajo de 0 se anticipa, por encima de 1 rebasa. Se aplica al soltar (un paso de deshacer)",
        ),
    ),
    (
        Msg::AnimCurvePreviewHover,
        t("Vista previa: el punto se mueve al ritmo de la curva, en bucle"),
    ),
    (
        Msg::AnimCurveShiftHint,
        t("Ajuste a cuartos y tercios · Mayús: mover libremente"),
    ),
    (Msg::AnimCurveSpeedInstant, t("instantánea")),
    (Msg::AnimCurveSpeedPerSecond, t("{value}/s")),
    (
        Msg::AnimGraphHandleOutHover,
        t(
            "Tirador de salida de los fotogramas {from}–{to}: influencia {influence} %, velocidad {speed}. Arrástrelo a los lados para la influencia y arriba o abajo para la velocidad; el suavizado de esta clave pasa a ser una curva personalizada (un paso de deshacer). Mayús: sin ajuste",
        ),
    ),
    (
        Msg::AnimGraphHandleInHover,
        t(
            "Tirador de llegada de los fotogramas {from}–{to}: influencia {influence} %, llega a {speed}. Arrástrelo a los lados para la influencia y arriba o abajo para la velocidad; el suavizado de esta clave pasa a ser una curva personalizada (un paso de deshacer). Mayús: sin ajuste",
        ),
    ),
    (
        Msg::AnimGraphSegmentHint,
        t(
            "Haga clic entre dos claves para seleccionar ese tramo; arrastre sus tiradores para dar forma a su curva.",
        ),
    ),
    (Msg::AnimElement, t("Elemento")),
    (Msg::AnimElementNumbered, t("Elemento {n}")),
    (Msg::AnimNoElements, t("No hay elementos que animar")),
    (
        Msg::AnimNothingAnimated,
        t(
            "Aún no hay nada animado en esta fase: use «+ Animar propiedad» debajo o un preajuste en el modo Simple",
        ),
    ),
    (
        Msg::AnimTracksHover,
        t(
            "Línea de tiempo: la regla mueve el cabezal y las filas contienen la animación de esta fase. Ctrl+rueda amplía, Mayús+rueda desplaza.",
        ),
    ),
    (
        Msg::AnimRulerHover,
        t(
            "Regla de tiempo: haga clic o arrastre para mover el cabezal (se ajusta a las claves; mantenga Mayús para recorrerla libremente). Ctrl+rueda amplía, Mayús+rueda desplaza.",
        ),
    ),
    (
        Msg::AnimPhaseEndHover,
        t(
            "Final de la fase ({length}): arrastre para estirar o comprimir todas las claves de esta fase. Mantenga Alt para cambiar la duración sin mover claves.",
        ),
    ),
    (
        Msg::AnimStripHover,
        t(
            "Tirador de tiempo: arrastre para estirar o comprimir en proporción las claves seleccionadas. Mantenga Alt para mover solo las claves de este borde; Mayús desactiva el ajuste.",
        ),
    ),
    (
        Msg::AnimKeyInfo,
        t(
            "{target} · clave de {property} en el fotograma {frame}: {value}, suavizado {easing} ({shape}).",
        ),
    ),
    (
        Msg::AnimKeyDragHelp,
        t(
            "Arrastre para mover las claves seleccionadas (se ajustan a claves, al cabezal y a segundos enteros; Mayús desactiva el ajuste). Ctrl+clic la añade o la quita de la selección; Mayús+clic selecciona un intervalo.",
        ),
    ),
    (Msg::AnimShapeLinear, t("rombo: lineal")),
    (Msg::AnimShapeHold, t("cuadrado: mantiene su valor")),
    (Msg::AnimShapeEased, t("círculo: suavizado")),
    (
        Msg::AnimBarBodyHover,
        t(
            "{name}: su animación en esta fase. Arrástrela para moverla en el tiempo; arrastre cualquiera de sus extremos para hacerla más rápida o más lenta. Haga clic para seleccionar sus claves.",
        ),
    ),
    (
        Msg::AnimBarEndHover,
        t(
            "Tirador de duración: arrastre para estirar o comprimir la animación de {name}. Mantenga Alt para mover solo este extremo; Mayús desactiva el ajuste.",
        ),
    ),
    (
        Msg::AnimRowEmptyHover,
        t(
            "Arrastre para seleccionar claves con un recuadro (Mayús añade, Ctrl alterna). Haga clic para vaciar la selección y convertir esta fila en la activa para K.",
        ),
    ),
    (
        Msg::AnimTargetEmptyHover,
        t(
            "{name} no tiene animación en esta fase. Elija un preajuste a la izquierda o pase al modo Avanzado para animar propiedades. Arrastre aquí para mover el cabezal.",
        ),
    ),
    (Msg::AnimRowLabelInfo, t("{target} · {property}: {help}.")),
    (
        Msg::AnimRowLabelClick,
        t(
            "Haga clic para convertirla en la fila activa: K le añade una clave en el cabezal y el gráfico de valores de abajo la muestra.",
        ),
    ),
    (
        Msg::AnimTargetLabelHover,
        t(
            "{name}: una barra muestra toda su animación en esta fase. Arrastre la barra para moverla y sus extremos para cambiar su tiempo.",
        ),
    ),
    (Msg::AnimPresetPick, t("Preajuste…")),
    (
        Msg::AnimPresetPickHover,
        t(
            "Elegir una animación lista para usar para {name} en esta fase. Sustituye las claves del elemento en las propiedades que usa el preajuste y dura lo que la fase (un solo paso de deshacer).",
        ),
    ),
    (
        Msg::AnimBarNoAnimation,
        t("Sin animación · elija un preajuste"),
    ),
    (Msg::AnimSnapPlayhead, t("ajuste: cabezal")),
    (Msg::AnimSnapKey, t("ajuste: clave")),
    (Msg::AnimSnapPhaseEdge, t("ajuste: borde de la fase")),
    (Msg::AnimSnapSecond, t("ajuste: segundo entero")),
    (
        Msg::AnimRetimeModifiers,
        t("Alt: mover un borde · Mayús: sin ajuste · Esc: cancelar"),
    ),
    // Colour grading panel (SW-019).
    (Msg::GradeTitle, t("Corrección de color")),
    (
        Msg::GradeTitleHover,
        t(
            "Corrección de color de esta imagen. El modo Simple tiene estilos y controles básicos; el Avanzado añade ruedas de color, curvas y una LUT. Haga clic dentro del panel para usar sus atajos de teclado.",
        ),
    ),
    (
        Msg::GradeToAdvancedHover,
        t("Cambiar al modo Avanzado: ruedas de color, curvas, curvas de tono y una LUT"),
    ),
    (
        Msg::GradeToSimpleHover,
        t("Cambiar al modo Simple: estilos y controles básicos"),
    ),
    (Msg::GradeBypass, t("Bypass")),
    (
        Msg::GradeBypassOffHover,
        t("Omitir la corrección para ver la imagen sin tocar; los ajustes se conservan"),
    ),
    (
        Msg::GradeBypassOnHover,
        t(
            "La corrección está omitida, así que la salida es la imagen sin tocar. Haga clic para reactivarla",
        ),
    ),
    (Msg::GradeAb, t("A/B")),
    (
        Msg::GradeAbHover,
        t(
            "Comparar dos correcciones: guarda esta como {slot} y cambia a la otra. La primera pulsación copia esta corrección para que pruebe una variante",
        ),
    ),
    (
        Msg::GradeCopyHover,
        t("Copiar toda la corrección para pegarla en otra imagen"),
    ),
    (
        Msg::GradePasteHover,
        t("Sustituir esta corrección por la copiada (se conserva el bypass)"),
    ),
    (
        Msg::GradeUndoHover,
        t("Deshacer la última edición de color (un arrastre completo es un paso)"),
    ),
    (
        Msg::GradeRedoHover,
        t("Rehacer la última edición de color deshecha"),
    ),
    (Msg::GradeResetAll, t("Restablecer todo")),
    (
        Msg::GradeResetAllHover,
        t("Devolver todos los ajustes a neutro y quitar la LUT (un solo paso de deshacer)"),
    ),
    (
        Msg::GradeReasonNothingToUndo,
        t("no hay ninguna edición de color que deshacer"),
    ),
    (
        Msg::GradeReasonNothingToRedo,
        t("no hay ninguna edición de color que rehacer"),
    ),
    (
        Msg::GradeReasonNothingCopied,
        t("aún no se ha copiado ninguna corrección; copie una primero"),
    ),
    (Msg::GradeReasonNeutral, t("la corrección ya es neutra")),
    (
        Msg::GradeReasonDefault,
        t("ya está en su valor predeterminado"),
    ),
    (Msg::GradeReasonNoLut, t("esta corrección no tiene LUT")),
    (
        Msg::GradeReasonNoLuts,
        t("el proyecto aún no tiene archivos LUT"),
    ),
    (
        Msg::GradeReasonNoPicture,
        t("aún no hay ninguna imagen que analizar"),
    ),
    (
        Msg::GradeErrInvalid,
        t("Ese valor está fuera del rango de la corrección"),
    ),
    (
        Msg::GradeErrEmptyClipboard,
        t("Aún no hay nada copiado: copie una corrección primero"),
    ),
    (
        Msg::GradeErrEmptySlot,
        t("Esa memoria de comparación está vacía"),
    ),
    (Msg::GradeErrCurveFull, t("Esta curva está llena")),
    (
        Msg::GradeErrMaxPoints,
        t("{message}: como máximo {max} puntos por curva."),
    ),
    (
        Msg::GradeErrPointTaken,
        t("Ya hay un punto ahí; arrástrelo"),
    ),
    (
        Msg::GradeErrEndpointLocked,
        t("Los extremos de una curva RGB se pueden mover pero no borrar"),
    ),
    (Msg::GradeErrUnknownPoint, t("Ese punto ya no existe")),
    (Msg::GradeLooks, t("Estilos")),
    (
        Msg::GradeLooksHover,
        t(
            "Estilos listos para usar, previsualizados sobre esta imagen. Haga clic en uno para aplicarlo.",
        ),
    ),
    (
        Msg::GradeLookApply,
        t(
            "Haga clic para aplicar este estilo (un solo paso de deshacer); se conservan la LUT y el bypass.",
        ),
    ),
    (Msg::GradeLookCurrent, t("Este estilo está aplicado ahora.")),
    (Msg::GradePresetNeutral, t("Neutro")),
    (Msg::GradePresetWarm, t("Cálido")),
    (Msg::GradePresetCool, t("Frío")),
    (Msg::GradePresetPunchy, t("Intenso")),
    (Msg::GradePresetFaded, t("Desvaído")),
    (Msg::GradePresetBw, t("B/N")),
    (
        Msg::GradePresetNeutralHover,
        t("Neutro: sin ninguna corrección"),
    ),
    (
        Msg::GradePresetWarmHover,
        t("Cálido: balance de blancos ámbar y algo más de color"),
    ),
    (
        Msg::GradePresetCoolHover,
        t("Frío: un balance de blancos más azul"),
    ),
    (
        Msg::GradePresetPunchyHover,
        t("Intenso: más contraste y color más fuerte"),
    ),
    (
        Msg::GradePresetFadedHover,
        t("Desvaído: negros levantados, contraste suave y color apagado"),
    ),
    (
        Msg::GradePresetBwHover,
        t("B/N: blanco y negro con un toque de contraste"),
    ),
    (Msg::GradeBasics, t("Básicos")),
    (
        Msg::GradeBasicsHover,
        t(
            "Las correcciones principales. Arrastre un control en horizontal (Mayús para ajuste fino); doble clic para restablecerlo.",
        ),
    ),
    (Msg::GradeExposure, t("Exposición")),
    (Msg::GradeContrast, t("Contraste")),
    (Msg::GradePivot, t("Pivote")),
    (Msg::GradeSaturation, t("Saturación")),
    (Msg::GradeTemperature, t("Temperatura")),
    (Msg::GradeTint, t("Matiz")),
    (Msg::GradeStrength, t("Intensidad")),
    (Msg::GradeMaster, t("Maestro")),
    (
        Msg::GradeExposureHover,
        t(
            "Exposición: aclara u oscurece toda la imagen en pasos, como abrir o cerrar el diafragma",
        ),
    ),
    (
        Msg::GradeContrastHover,
        t(
            "Contraste: aleja los tonos del pivote, oscureciendo las sombras y aclarando las altas luces",
        ),
    ),
    (
        Msg::GradePivotHover,
        t("Pivote: el tono alrededor del que gira el contraste; conserva su brillo"),
    ),
    (
        Msg::GradeSaturationHover,
        t("Saturación: cuánto color tiene la imagen; cero es blanco y negro"),
    ),
    (
        Msg::GradeTemperatureHover,
        t("Temperatura: balance de blancos de más frío (azul) a más cálido (ámbar)"),
    ),
    (
        Msg::GradeTintHover,
        t("Matiz: balance de blancos de verde a magenta, para dominantes de fluorescentes o LED"),
    ),
    (
        Msg::GradeStrengthHover,
        t("Intensidad: cuánto de la LUT se mezcla"),
    ),
    (
        Msg::GradeSliderHelp,
        t(
            "Ahora {value}. Arrastre en horizontal para ajustar (Mayús: fino); doble clic para volver a {default}.",
        ),
    ),
    (Msg::GradeAutoBalance, t("Balance automático")),
    (
        Msg::GradeAutoBalanceHover,
        t(
            "Ajustar temperatura y matiz para que el color medio de la imagen sea gris neutro (un solo paso de deshacer)",
        ),
    ),
    (Msg::GradeWheels, t("Ruedas de color")),
    (
        Msg::GradeWheelsHover,
        t(
            "Teñir y aclarar las sombras (Lift), los medios tonos (Gamma), las altas luces (Gain) o todos los tonos (Offset)",
        ),
    ),
    (Msg::GradeLift, t("Lift")),
    (Msg::GradeGamma, t("Gamma")),
    (Msg::GradeGain, t("Gain")),
    (Msg::GradeOffset, t("Offset")),
    (
        Msg::GradeLiftHover,
        t(
            "Lift: el color de las sombras. Arrastre el punto hacia un tono para teñir los tonos oscuros.",
        ),
    ),
    (
        Msg::GradeGammaHover,
        t("Gamma: el color de los medios tonos. Arrastre el punto hacia un tono para teñirlos."),
    ),
    (
        Msg::GradeGainHover,
        t("Gain: el color de las altas luces. Arrastre el punto hacia un tono para teñirlas."),
    ),
    (
        Msg::GradeOffsetHover,
        t(
            "Offset: el color de todos los tonos a la vez. Arrastre el punto hacia un tono para teñir toda la imagen.",
        ),
    ),
    (
        Msg::GradeWheelHelp,
        t(
            "Ahora {angle}°, {amount}. Arrastre en cualquier punto de la rueda (Mayús: fino); doble clic para centrar.",
        ),
    ),
    (
        Msg::GradeWheelNeutralHelp,
        t(
            "Centrada (sin tinte). Arrastre en cualquier punto de la rueda (Mayús: fino); doble clic para centrar.",
        ),
    ),
    (
        Msg::GradeLiftMasterHover,
        t("Maestro de Lift: sube o baja el nivel de negro"),
    ),
    (
        Msg::GradeGammaMasterHover,
        t("Maestro de Gamma: aclara u oscurece los medios tonos"),
    ),
    (
        Msg::GradeGainMasterHover,
        t("Maestro de Gain: aclara u oscurece las altas luces"),
    ),
    (
        Msg::GradeOffsetMasterHover,
        t("Maestro de Offset: sube o baja todos los tonos por igual"),
    ),
    (
        Msg::GradeWheelResetHover,
        t(
            "Restablecer {wheel}: centrar la rueda y poner su maestro a cero (un solo paso de deshacer)",
        ),
    ),
    (Msg::GradeCurves, t("Curvas")),
    (
        Msg::GradeCurvesHover,
        t(
            "Curvas de tono y de color. Elija una curva con las pestañas y dele forma en el gráfico.",
        ),
    ),
    (Msg::GradeCurveMaster, t("RGB")),
    (
        Msg::GradeCurveMasterHover,
        t("Curva maestra: da forma al brillo del rojo, el verde y el azul a la vez"),
    ),
    (
        Msg::GradeCurveRedHover,
        t("Curva roja: más o menos rojo en cada tono"),
    ),
    (
        Msg::GradeCurveGreenHover,
        t("Curva verde: más o menos verde en cada tono"),
    ),
    (
        Msg::GradeCurveBlueHover,
        t("Curva azul: más o menos azul en cada tono"),
    ),
    (Msg::GradeHueVsHue, t("Tono/tono")),
    (Msg::GradeHueVsSat, t("Tono/sat.")),
    (Msg::GradeLumaVsSat, t("Luma/sat.")),
    (
        Msg::GradeHueVsHueHover,
        t(
            "Tono/tono: convierte una gama de colores en otra; suba o baje un punto para girar ese tono",
        ),
    ),
    (
        Msg::GradeHueVsSatHover,
        t("Tono/sat.: hace una gama de colores más o menos saturada"),
    ),
    (
        Msg::GradeLumaVsSatHover,
        t("Luma/sat.: hace las sombras, los medios tonos o las altas luces más o menos saturados"),
    ),
    (
        Msg::GradeCurveCanvasHover,
        t(
            "Haga clic para añadir un punto y arrastre los puntos para moverlos (Mayús: fino). Doble clic o clic derecho en un punto para borrarlo.",
        ),
    ),
    (Msg::GradeCurveReset, t("Restablecer curva")),
    (
        Msg::GradeCurveResetHover,
        t("Restablecer la curva {curve} a neutra (un solo paso de deshacer)"),
    ),
    (
        Msg::GradeCurveEmpty,
        t("Plana: haga clic en la línea para añadir un punto"),
    ),
    (Msg::GradeLut, t("LUT")),
    (
        Msg::GradeLutHover,
        t("LUT: una tabla de consulta (archivo .cube) aplicada después de las demás correcciones"),
    ),
    (Msg::GradeLutNone, t("Sin LUT")),
    (
        Msg::GradeLutPickHover,
        t("Elegir una LUT de los archivos LUT del proyecto (un solo paso de deshacer)"),
    ),
    (Msg::GradeLutRemove, t("Quitar")),
    (
        Msg::GradeLutRemoveHover,
        t("Quitar la LUT de esta corrección (un solo paso de deshacer)"),
    ),
    (
        Msg::GradeLutMissing,
        t("LUT no cargada: la vista previa muestra la corrección sin ella"),
    ),
    (
        Msg::GradePreviewHover,
        t("Esta imagen con la corrección aplicada"),
    ),
    (Msg::GradeCompareOff, t("Corregida")),
    (Msg::GradeCompareSide, t("Lado a lado")),
    (
        Msg::GradeCompareOffHover,
        t("Mostrar solo la imagen corregida"),
    ),
    (
        Msg::GradeCompareWipeHover,
        t("Mostrar el original a la izquierda de una línea y la imagen corregida a la derecha"),
    ),
    (
        Msg::GradeCompareSideHover,
        t("Mostrar el original y la imagen corregida una junto a la otra"),
    ),
    (
        Msg::GradeWipeHandleHover,
        t(
            "Línea de cortina: arrástrela para comparar el original (izquierda) con la corrección (derecha); doble clic para centrarla",
        ),
    ),
    (Msg::GradeOriginal, t("ORIGINAL")),
    (Msg::GradeGraded, t("CORREGIDA")),
    (Msg::GradeBypassed, t("OMITIDA")),
    (
        Msg::GradeNoPicture,
        t("Aún no hay imagen para previsualizar"),
    ),
    (Msg::GradeScopes, t("Visores")),
    (
        Msg::GradeScopesHover,
        t("Los visores miden la imagen corregida, para corregir con datos y no solo a ojo"),
    ),
    (Msg::GradeScopeWaveform, t("Forma de onda")),
    (Msg::GradeScopeParade, t("Desfile RGB")),
    (Msg::GradeScopeVectorscope, t("Vectorscopio")),
    (Msg::GradeScopeHistogram, t("Histograma")),
    (
        Msg::GradeScopeWaveformHover,
        t("Forma de onda: el brillo de cada columna de la imagen, negro abajo y blanco arriba"),
    ),
    (
        Msg::GradeScopeParadeHover,
        t(
            "Desfile RGB: formas de onda roja, verde y azul una junto a otra; niveles iguales indican grises neutros",
        ),
    ),
    (
        Msg::GradeScopeVectorscopeHover,
        t(
            "Vectorscopio: el tono alrededor del círculo y la saturación hacia fuera desde el centro gris; las cajas marcan barras al 75 %",
        ),
    ),
    (
        Msg::GradeScopeHistogramHover,
        t(
            "Histograma: cuántos píxeles hay en cada nivel, negro a la izquierda y blanco a la derecha",
        ),
    ),
    (
        Msg::GradeScopeShowHover,
        t("Haga clic para mostrar este visor."),
    ),
    (
        Msg::GradeCurveShowHover,
        t("Haga clic para editar esta curva."),
    ),
    // Studio Colour window (SW-019).
    (Msg::ColorMenuItem, t("Corrección de color…")),
    (
        Msg::ColorMenuHover,
        t("Abrir la ventana Color para corregir una entrada o el Programa"),
    ),
    (Msg::ColorTileItem, t("Corregir color…")),
    (
        Msg::ColorTileHover,
        t("Abrir esta entrada en la ventana Color para corregirla"),
    ),
    (Msg::ShortcutColor, t("Abrir o cerrar la ventana Color")),
    (Msg::ColorTarget, t("Destino")),
    (
        Msg::ColorTargetHover,
        t(
            "Qué corregir: una entrada (antes de mezclar, dondequiera que aparezca), una capa de escena (dentro de su escena) o el Programa (después de las superposiciones, antes del fundido a negro)",
        ),
    ),
    (Msg::ColorTargetProgram, t("Salida de Programa")),
    (
        Msg::ColorTargetProgramHover,
        t("Corregir la imagen compuesta de Programa"),
    ),
    (
        Msg::ColorTargetInputHover,
        t("Corregir esta entrada dondequiera que aparezca"),
    ),
    (
        Msg::ColorTargetLayerHover,
        t(
            "Corregir solo esta capa de escena: después de la corrección y los efectos de su entrada, antes de su posición y tamaño. Se ve dondequiera que se dibuje la escena (una entrada de escena, una salida u otra escena que la anide)",
        ),
    ),
    (Msg::ColorTargetLayerFallback, t("Capa")),
    (Msg::ColorTargetSceneFallback, t("Escena")),
    (Msg::ColorTargetSceneUnshown, t("sin entrada de escena")),
    (Msg::ColorGraded, t("corregida")),
    (
        Msg::ColorStatusOnAirHover,
        t("El servidor aplicó esta corrección; los monitores la muestran"),
    ),
    (Msg::ColorStatusEditing, t("EDITANDO")),
    (
        Msg::ColorStatusEditingHover,
        t("El cambio se envía mientras arrastra y otra vez al soltar"),
    ),
    (Msg::ColorStatusSending, t("ENVIANDO")),
    (
        Msg::ColorStatusSendingHover,
        t("Enviada; esperando a que el servidor la aplique"),
    ),
    (Msg::ColorStatusRefused, t("RECHAZADA")),
    (
        Msg::ColorStatusRefusedHover,
        t(
            "El servidor rechazó esta corrección, así que no está al aire (el aviso dice por qué). Reviértala o cámbiela para enviarla de nuevo",
        ),
    ),
    (Msg::ColorStatusDiverged, t("NO AL AIRE")),
    (
        Msg::ColorStatusDivergedHover,
        t(
            "Este borrador difiere de la corrección al aire y no se aplicó. Reviértalo o cámbielo para enviarlo de nuevo",
        ),
    ),
    (Msg::ColorRevert, t("Revertir")),
    (
        Msg::ColorRevertHover,
        t("Descartar este borrador y volver a la corrección al aire"),
    ),
    (
        Msg::ColorReasonOnAir,
        t("el borrador ya es la corrección al aire"),
    ),
    (
        Msg::ColorReasonGradingPending,
        t("las correcciones del proyecto aún se están sincronizando"),
    ),
    (
        Msg::ColorClose,
        t("Cerrar la ventana Color; un cambio que aún se envía se envía igualmente"),
    ),
    (Msg::ColorLibrary, t("Biblioteca")),
    (
        Msg::ColorLibraryHover,
        t(
            "Correcciones con nombre guardadas en el proyecto. Un destino que aplica una sigue sus actualizaciones posteriores",
        ),
    ),
    (
        Msg::ColorLibraryPickHover,
        t("Elegir una corrección de la biblioteca para aplicarla"),
    ),
    (
        Msg::ColorLibraryNone,
        t("Aún no hay correcciones en la biblioteca"),
    ),
    (Msg::ColorLibraryApply, t("Aplicar")),
    (
        Msg::ColorLibraryApplyHover,
        t(
            "Aplicar la corrección elegida a este destino (vinculada: sigue las actualizaciones posteriores de la biblioteca)",
        ),
    ),
    (
        Msg::ColorReasonNoLibrary,
        t("la biblioteca del proyecto aún no tiene correcciones; guarde una primero"),
    ),
    (
        Msg::ColorReasonPickLibrary,
        t("elija primero una corrección de la biblioteca"),
    ),
    (Msg::ColorLibraryNameHint, t("Nombre de esta corrección")),
    (
        Msg::ColorLibraryNameHover,
        t("Nombre de la nueva corrección de biblioteca: único, de 128 bytes como máximo"),
    ),
    (Msg::ColorLibrarySave, t("Guardar en biblioteca")),
    (
        Msg::ColorLibrarySaveHover,
        t("Guardar el borrador actual en la biblioteca del proyecto con este nombre"),
    ),
    (Msg::ColorReasonNameBlank, t("escriba primero un nombre")),
    (
        Msg::ColorReasonNameControl,
        t("el nombre contiene caracteres de control"),
    ),
    (
        Msg::ColorReasonNameTaken,
        t("otra corrección de la biblioteca ya tiene este nombre"),
    ),
    (
        Msg::ColorReasonLibraryFull,
        t("la biblioteca del proyecto está llena"),
    ),
    (Msg::ColorLut, t("Importar LUT")),
    (
        Msg::ColorLutHover,
        t("Importar una LUT .cube al proyecto; después elíjala en el modo Avanzado, en LUT"),
    ),
    (Msg::ColorLutPathHint, t("Ruta de un archivo .cube")),
    (
        Msg::ColorLutPathHover,
        t("Ruta de un archivo .cube en este equipo. Studio envía su texto, de 32 KiB como máximo"),
    ),
    (Msg::ColorLutImport, t("Importar")),
    (
        Msg::ColorLutImportHover,
        t("Leer el archivo e importarlo como LUT del proyecto con el nombre del archivo"),
    ),
    (
        Msg::ColorReasonNoPath,
        t("escriba primero la ruta de un archivo .cube"),
    ),
    (
        Msg::ColorReasonLutNameTaken,
        t("el proyecto ya tiene una LUT con este nombre de archivo"),
    ),
    (
        Msg::ColorReasonLutsFull,
        t("el proyecto no tiene ningún id de LUT libre"),
    ),
    (
        Msg::ColorLutLimit,
        t(
            "Hasta 32 KiB por la conexión (LUT 1D y 3D hasta unos 17³). Importe LUT mayores con freemix-cli grade-import-lut.",
        ),
    ),
    (
        Msg::ColorLutTooLarge,
        t(
            "{file} ocupa {size} KiB; Studio envía LUT de 32 KiB como máximo. Impórtela sin conexión con freemix-cli grade-import-lut.",
        ),
    ),
    (
        Msg::ColorLutUnreadable,
        t("No se pudo leer {file}: {error}"),
    ),
    (
        Msg::ColorLutNotText,
        t("{file} no es un archivo .cube de texto"),
    ),
    (
        Msg::ColorLutInvalid,
        t("{file} no es una LUT .cube válida: {error}"),
    ),
    (Msg::ColorLutImporting, t("Importando la LUT {name}…")),
    (
        Msg::ColorSourceLive,
        t("Imagen: fotograma en vivo del monitor {tap}"),
    ),
    (
        Msg::ColorSourceHeld,
        t(
            "Imagen: último fotograma sin corregir de esta fuente; los monitores muestran la corrección al aire",
        ),
    ),
    (
        Msg::ColorSourceStill,
        t(
            "Imagen: imagen de prueba. No ha llegado ningún fotograma sin corregir de esta fuente; póngala en Previo para verla en vivo",
        ),
    ),
    (
        Msg::ColorSourceLayerStill,
        t(
            "Imagen: imagen de prueba. No ha llegado ningún fotograma de la fuente de esta capa; ponga su entrada en Previo para ver la capa en vivo",
        ),
    ),
    (
        Msg::ColorSourceHover,
        t(
            "La vista previa corrige esta imagen en este equipo, así que arrastrar es instantáneo; el servidor aplica la corrección a los monitores",
        ),
    ),
    // Studio Animation window (GX-011).
    (Msg::AnimMenuItem, t("Animar título…")),
    (
        Msg::AnimMenuHover,
        t("Abrir la ventana Animación para animar los elementos de un título"),
    ),
    (
        Msg::ShortcutAnimation,
        t("Abrir o cerrar la ventana Animación"),
    ),
    (Msg::TitleAnimate, t("Animar…")),
    (
        Msg::TitleAnimateHover,
        t(
            "Abrir este título en la ventana Animación: un preajuste por elemento, fotogramas clave y una vista previa exacta",
        ),
    ),
    (Msg::AnimWinSection, t("Animar")),
    (
        Msg::AnimWinSectionHover,
        t(
            "La animación de entrada y salida de los elementos de un título. Se guarda en la plantilla del título, así que la reproduce cada entrada que usa la plantilla",
        ),
    ),
    (Msg::AnimWinTitleHover, t("Qué título animar")),
    (Msg::AnimWinTitleOptionHover, t("Animar {name}")),
    (
        Msg::AnimWinNoTitles,
        t("Ningún título ni escena que animar"),
    ),
    (
        Msg::AnimWinNoTitlesDetail,
        t(
            "Añada una entrada de título o de escena al show y elija Animar… en la fila de un título de la pestaña Títulos o elija aquí una escena.",
        ),
    ),
    (Msg::AnimWinTargetHover, t("Qué título o escena animar")),
    (Msg::AnimWinGroupScenes, t("Escenas")),
    (
        Msg::AnimWinSceneOptionHover,
        t("Animar las capas de la escena {name}"),
    ),
    (
        Msg::AnimSceneSectionHover,
        t(
            "La animación de entrada y salida de las capas de una escena. Se guarda en el proyecto, así que la reproduce cada entrada que muestra la escena: la entrada cuando la escena llega a Programa o Vista previa, la salida mientras una transición la saca de Programa (un corte no tiene salida)",
        ),
    ),
    (
        Msg::AnimSceneCanvasHover,
        t(
            "Las capas de la escena en el cabezal. Desplácese por la regla de la línea de tiempo; arrastre claves o barras para reajustar el tiempo",
        ),
    ),
    (
        Msg::AnimReasonScenePending,
        t("la animación de la escena aún se está sincronizando"),
    ),
    (Msg::AnimStatusSaved, t("GUARDADO")),
    (
        Msg::AnimStatusSavedHover,
        t("El servidor tiene esta animación; la próxima toma la reproduce"),
    ),
    (Msg::AnimStatusUnsaved, t("SIN GUARDAR")),
    (
        Msg::AnimStatusUnsavedHover,
        t(
            "Cambios aún sin guardar. Con el guardado automático se guardan al soltar un arrastre; si no, pulse Guardar",
        ),
    ),
    (Msg::AnimStatusSaving, t("GUARDANDO")),
    (
        Msg::AnimStatusSavingHover,
        t("Se envió su cambio; esperando a que el servidor lo confirme"),
    ),
    (
        Msg::AnimStatusConflictHover,
        t(
            "Otra persona cambió esta animación mientras usted la editaba. Recargue la suya o sobrescríbala con la de usted",
        ),
    ),
    (
        Msg::AnimStatusRefusedHover,
        t(
            "El servidor rechazó el último guardado, así que no sale al aire. Edite de nuevo o pulse Guardar para reintentar",
        ),
    ),
    (Msg::AnimAutoSave, t("Guardado automático")),
    (
        Msg::AnimAutoSaveHover,
        t(
            "Guardar cada cambio en cuanto lo termina (al soltar el ratón). Desactivado: los cambios se quedan en este equipo hasta que pulse Guardar",
        ),
    ),
    (
        Msg::AnimSaveHover,
        t("Guardar ahora esta animación en la plantilla del título"),
    ),
    (
        Msg::AnimRevertHover,
        t("Descartar los cambios sin guardar y volver a la animación guardada"),
    ),
    (Msg::AnimReasonSaved, t("no hay cambios sin guardar")),
    (Msg::AnimReasonSaving, t("ya hay un guardado en camino")),
    (
        Msg::AnimReasonConflict,
        t("resuelva primero el conflicto: recargue la suya o sobrescriba"),
    ),
    (
        Msg::AnimReasonAnimationPending,
        t("la animación del título aún se está sincronizando"),
    ),
    (
        Msg::AnimReasonPreviewTransport,
        t("las vistas previas exactas necesitan una conexión TCP directa con el servidor"),
    ),
    (Msg::AnimOnion, t("Papel cebolla")),
    (
        Msg::AnimOnionHover,
        t(
            "Mostrar contornos tenues de los elementos unos fotogramas antes (azul) y después (ámbar) del cabezal, para leer el movimiento de un vistazo",
        ),
    ),
    (
        Msg::AnimConflictBanner,
        t("Otra persona cambió esta animación mientras usted la editaba."),
    ),
    (Msg::AnimReloadTheirs, t("Recargar la suya")),
    (
        Msg::AnimReloadTheirsHover,
        t("Descartar sus cambios y cargar la animación que tiene ahora el servidor"),
    ),
    (Msg::AnimOverwrite, t("Sobrescribir")),
    (
        Msg::AnimOverwriteHover,
        t("Conservar su versión y guardarla encima de la otra"),
    ),
    (Msg::AnimClose, t("Cerrar la ventana Animación")),
    (Msg::AnimBadgeExact, t("VISTA EXACTA")),
    (
        Msg::AnimBadgeExactHover,
        t("Renderizada por el servidor tal como saldrá al aire este fotograma"),
    ),
    (Msg::AnimBadgeFast, t("VISTA RÁPIDA")),
    (
        Msg::AnimBadgeFastHover,
        t(
            "Contornos de los elementos dibujados en este equipo para que arrastrar sea instantáneo; el fotograma exacto llega cuando se detiene",
        ),
    ),
    (
        Msg::AnimBadgeProxyHover,
        t("Contornos de los elementos dibujados en este equipo. {reason}"),
    ),
    (
        Msg::AnimCanvasHover,
        t(
            "El título en el cabezal. Recorra la regla de la línea de tiempo; arrastre claves o barras para cambiar los tiempos",
        ),
    ),
    (
        Msg::AnimCanvasCaption,
        t("{phase} · fotograma {frame} de {total}"),
    ),
    (
        Msg::AnimCanvasEmpty,
        t("Esperando la primera vista previa…"),
    ),
    (
        Msg::AnimDesignUnavailable,
        t(
            "Este título no tiene disposición de elementos (su plantilla no se cargó): solo se listan los elementos que ya se mueven, sus preajustes se aplican en el servidor y no se dibujan contornos.",
        ),
    ),
    (
        Msg::AnimReasonServerPreset,
        t(
            "guarde o revierta primero sus cambios: los preajustes de este elemento se aplican en el servidor",
        ),
    ),
    (
        Msg::AnimPreviewUnavailable,
        t("Las vistas previas exactas no están disponibles: {reason}."),
    ),
    (Msg::AnimDock, t("Acoplar")),
    (
        Msg::AnimDockHover,
        t(
            "Acoplar la ventana Animación en la parte inferior de la consola; arrastre su borde superior para cambiar su altura",
        ),
    ),
    (Msg::AnimFloat, t("Flotar")),
    (
        Msg::AnimFloatHover,
        t(
            "Hacer flotar la ventana Animación sobre la consola; arrastre sus bordes o su esquina para cambiar su tamaño",
        ),
    ),
    (Msg::AnimBadgePreset, t("VISTA DE PREAJUSTE")),
    (
        Msg::AnimBadgePresetHover,
        t(
            "{preset} en {name}, solo como vista previa: los contornos muestran su movimiento al instante y el fotograma exacto llega cuando se detiene. Haga clic en el preajuste para aplicarlo (un paso de deshacer); aléjese para cancelar",
        ),
    ),
    (
        Msg::AnimPresetItemHover,
        t(
            "{help}\nDeténgase aquí para verlo en el lienzo; haga clic para aplicarlo (un paso de deshacer)",
        ),
    ),
    (Msg::AnimTargetKindHover, t("Tipo: {kind}")),
    (Msg::AnimKindText, t("Texto")),
    (Msg::AnimKindShape, t("Forma")),
    (Msg::AnimKindImage, t("Imagen")),
    (Msg::AnimKindVideo, t("Vídeo")),
    (Msg::AnimKindGroup, t("Grupo")),
    (Msg::AnimKindOther, t("Otro elemento")),
    // Studio layer designer (SW-006).
    (Msg::DesignMenuItem, t("Diseñar la escena…")),
    (
        Msg::DesignMenuHover,
        t(
            "Abrir el diseñador de capas: mover, escalar, recortar, girar y apilar las capas de una escena sobre su lienzo, o escribir valores exactos",
        ),
    ),
    (
        Msg::ShortcutSceneDesigner,
        t("Abrir o cerrar el diseñador de capas"),
    ),
    (Msg::DesignOpen, t("Diseñar capas…")),
    (
        Msg::DesignOpenHover,
        t(
            "Abrir esta escena en el diseñador de capas para mover, escalar, recortar y apilar sus capas",
        ),
    ),
    (Msg::DesignTileItem, t("Diseñar…")),
    (
        Msg::DesignTileHover,
        t(
            "Abrir la escena de esta entrada en el diseñador de capas: mover, escalar, girar, recortar y apilar sus capas en vivo",
        ),
    ),
    (Msg::DesignTitle, t("Diseñador de capas")),
    (
        Msg::DesignTitleHover,
        t(
            "Organice las capas de una escena en vivo: arrastre en el lienzo o escriba valores exactos. El servidor recibe cada cambio al hacerlo, y un arrastre o cambio de campo completo de una capa es un paso del deshacer del show",
        ),
    ),
    (Msg::DesignScenePickHover, t("Qué escena diseñar")),
    (
        Msg::DesignSceneOptionHover,
        t("Diseñar las capas de la escena {name}"),
    ),
    (Msg::DesignSceneFallback, t("Escena {id}")),
    (Msg::DesignNoScenes, t("Ninguna escena que diseñar")),
    (
        Msg::DesignNoScenesDetail,
        t(
            "El show aún no tiene escenas. Añada una entrada de escena sin conexión (freemix-cli scene-input-add) y vuelva a abrir el diseñador.",
        ),
    ),
    (
        Msg::DesignWaiting,
        t("Esperando la composición de la escena…"),
    ),
    (Msg::DesignClose, t("Cerrar el diseñador de capas")),
    (
        Msg::DesignUndoHover,
        t(
            "Deshacer la última edición del proyecto en el servidor (un arrastre o cambio de campo completo de una capa es un paso)",
        ),
    ),
    (
        Msg::DesignRedoHover,
        t("Rehacer la edición del proyecto deshecha la última en el servidor"),
    ),
    (Msg::DesignStatusMatches, t("EDICIÓN EN VIVO")),
    (
        Msg::DesignStatusMatchesHover,
        t("Cada cambio va al servidor al hacerlo; el lienzo muestra la composición del servidor"),
    ),
    (Msg::DesignStatusSending, t("ENVIANDO CAMBIOS")),
    (
        Msg::DesignStatusSendingHover,
        t("Sus últimos cambios van de camino al servidor"),
    ),
    (
        Msg::DesignBaseChanged,
        t(
            "La composición de esta escena cambió en el servidor mientras sus cambios aún no se habían enviado. Se envían a continuación; después la composición del servidor manda.",
        ),
    ),
    (Msg::DesignCopyCli, t("Copiar como comandos")),
    (
        Msg::DesignCopyCliHover,
        t(
            "Copiar la composición completa de esta escena como comandos scene-layer de freemix-cli para {project}, para reproducirla sin conexión en un paquete de proyecto",
        ),
    ),
    (
        Msg::DesignCopied,
        plural(
            "Copiado {count} comando de freemix-cli de la composición",
            "Copiados {count} comandos de freemix-cli de la composición",
        ),
    ),
    (Msg::DesignSnap, t("Ajustar")),
    (
        Msg::DesignSnapHover,
        t(
            "Ajustar bordes y centros movidos al lienzo, a las zonas seguras y a otras capas, con guías. Mantenga Mayús al mover, o Ctrl en cualquier arrastre, para colocar libremente",
        ),
    ),
    (Msg::DesignSafeAreas, t("Zonas seguras")),
    (
        Msg::DesignSafeAreasHover,
        t("Mostrar las zonas seguras de acción (93%) y de títulos (90%) y ajustar a ellas"),
    ),
    (Msg::DesignSnapLayers, t("Bordes de capas")),
    (
        Msg::DesignSnapLayersHover,
        t("Ajustar a los bordes y centros de las demás capas"),
    ),
    (
        Msg::DesignCanvasHover,
        t(
            "Haga clic en una capa para seleccionarla (Mayús añade). Arrastre para mover; los tiradores de esquina y de borde escalan (Mayús mantiene la proporción); el tirador superior la gira respecto a su ancla a cualquier ángulo (Mayús: pasos de 15°); Alt y arrastrar la marca del ancla la mueve; Alt y arrastrar un borde recorta. Las flechas desplazan 1 px (Mayús: 10 px); Esc cancela un arrastre. Cada cambio va al servidor al hacerlo",
        ),
    ),
    (Msg::DesignBadgeExact, t("IMAGEN DEL SERVIDOR")),
    (
        Msg::DesignBadgeExactHover,
        t(
            "La composición de la escena en el servidor, con sus fuentes en vivo; los contornos siguen sus ediciones al momento",
        ),
    ),
    (Msg::DesignBadgeNone, t("SOLO CONTORNOS")),
    (
        Msg::DesignBadgeNoneHover,
        t("Sin imagen exacta de la escena: {reason}"),
    ),
    (
        Msg::DesignReasonNoSceneInput,
        t("ninguna entrada de escena muestra esta escena"),
    ),
    (
        Msg::DesignReasonWaitingPicture,
        t("la primera imagen está en camino"),
    ),
    (Msg::DesignLayers, t("Capas")),
    (
        Msg::DesignLayersHover,
        t(
            "Las capas de la escena, la superior primero. Haga clic para seleccionar (Mayús o Ctrl añade); arrastre una fila para reapilar",
        ),
    ),
    (
        Msg::DesignLayerRowHover,
        t("{name} · {source} · posición {position} de {count} en la pila. Arrastre para reapilar"),
    ),
    (Msg::DesignLayerUnnamed, t("Capa {id}")),
    (Msg::DesignSourceInput, t("entrada {name}")),
    (Msg::DesignSourceScene, t("escena {name}")),
    (Msg::DesignForward, t("Adelante")),
    (
        Msg::DesignForwardHover,
        t("Subir las capas seleccionadas un puesto en la pila"),
    ),
    (Msg::DesignBackward, t("Atrás")),
    (
        Msg::DesignBackwardHover,
        t("Bajar las capas seleccionadas un puesto en la pila"),
    ),
    (Msg::DesignToFront, t("Al frente")),
    (
        Msg::DesignToFrontHover,
        t("Subir las capas seleccionadas a lo más alto de la pila"),
    ),
    (Msg::DesignToBack, t("Al fondo")),
    (
        Msg::DesignToBackHover,
        t("Bajar las capas seleccionadas al fondo de la pila"),
    ),
    (
        Msg::DesignReasonNoSelection,
        t("seleccione primero una capa"),
    ),
    (
        Msg::DesignReasonTop,
        t("la selección ya está arriba del todo"),
    ),
    (Msg::DesignReasonBottom, t("la selección ya está al fondo")),
    (Msg::DesignInspector, t("Inspector")),
    (
        Msg::DesignInspectorHover,
        t(
            "Valores exactos de la capa seleccionada. Con varias seleccionadas, un cambio se aplica a todas. Arrastre un valor para variarlo o haga clic para escribirlo",
        ),
    ),
    (
        Msg::DesignNothingSelected,
        t("Seleccione una capa en el lienzo o en la lista."),
    ),
    (
        Msg::DesignMultiSelected,
        plural("{count} capa seleccionada", "{count} capas seleccionadas"),
    ),
    (Msg::DesignGroupPosition, t("Posición")),
    (Msg::DesignGroupSize, t("Tamaño")),
    (Msg::DesignGroupLook, t("Aspecto")),
    (Msg::DesignGroupCrop, t("Recorte")),
    (Msg::DesignGroupMask, t("Máscara")),
    (Msg::DesignGroupStack, t("Pila")),
    (Msg::DesignGroupAnchor, t("Ancla")),
    (
        Msg::DesignXHover,
        t("Desplazamiento X: el borde izquierdo de la capa en el lienzo, en píxeles"),
    ),
    (
        Msg::DesignYHover,
        t("Desplazamiento Y: el borde superior de la capa en el lienzo, en píxeles"),
    ),
    (
        Msg::DesignWidthHover,
        t("Ancho de la capa antes de girarla, en píxeles"),
    ),
    (
        Msg::DesignHeightHover,
        t("Alto de la capa antes de girarla, en píxeles"),
    ),
    (
        Msg::DesignZoomHover,
        t(
            "Zoom: el ancho de la capa como porcentaje del ancho del lienzo. Al cambiarlo se escalan ambos lados respecto al ancla",
        ),
    ),
    (
        Msg::DesignRotationHover,
        t(
            "Girar la capa en sentido horario respecto a su ancla, a cualquier ángulo (hasta tres decimales)",
        ),
    ),
    (Msg::DesignRotationField, t("Ángulo")),
    (
        Msg::DesignRotationOptionHover,
        t("Girar la capa a {angle}° en sentido horario respecto a su ancla"),
    ),
    (
        Msg::DesignOpacityHover,
        t("Opacidad de la capa, en porcentaje"),
    ),
    (Msg::DesignVisible, t("Visible")),
    (Msg::DesignVisibleHover, t("Si la capa se dibuja o no")),
    (
        Msg::DesignCropHover,
        t(
            "Recortar estos píxeles de la fuente por el borde {edge}; la imagen recortada llena la capa. Alt y arrastrar un tirador de borde en el lienzo recorta a mano",
        ),
    ),
    (Msg::DesignEdgeLeft, t("izquierdo")),
    (Msg::DesignEdgeTop, t("superior")),
    (Msg::DesignEdgeRight, t("derecho")),
    (Msg::DesignEdgeBottom, t("inferior")),
    (
        Msg::DesignMaskHover,
        t("Mostrar solo un rectángulo de la imagen recortada (invertida: todo menos él)"),
    ),
    (Msg::DesignMaskInvert, t("Invertir")),
    (
        Msg::DesignMaskInvertHover,
        t("Conservar lo que queda fuera del rectángulo de la máscara"),
    ),
    (
        Msg::DesignMaskFieldHover,
        t("{field} del rectángulo de la máscara, en píxeles de la fuente recortada"),
    ),
    (Msg::DesignReasonNoMask, t("active primero la máscara")),
    (
        Msg::DesignReasonTooManyVisible,
        t("la escena ya dibuja 64 capas visibles, el máximo posible; oculte una primero"),
    ),
    (Msg::DesignStackPosition, t("{position} de {count}")),
    (
        Msg::DesignStackHover,
        t(
            "Dónde está la capa en la lista (1 es el fondo). Las capas se dibujan por orden z y luego por esta posición; reapile con los botones o arrastrando filas en la lista",
        ),
    ),
    (Msg::DesignZOrder, t("Orden z")),
    (
        Msg::DesignZOrderHover,
        t(
            "Orden de dibujo: una capa con un orden z mayor se dibuja sobre las menores; con valores iguales, en el orden de la lista",
        ),
    ),
    (Msg::DesignAnchorCentre, t("Centro")),
    (
        Msg::DesignAnchorHover,
        t(
            "El punto respecto al que la capa gira y hace zoom, en porcentaje de su ancho y alto desde la esquina superior izquierda, marcado en el lienzo. Moverlo deja la capa donde está; en el lienzo, Alt y arrastrar la marca",
        ),
    ),
    (
        Msg::DesignAnchorXHover,
        t("Ancla a lo ancho de la capa, en porcentaje desde su borde izquierdo (antes de girarla)"),
    ),
    (
        Msg::DesignAnchorYHover,
        t("Ancla a lo alto de la capa, en porcentaje desde su borde superior (antes de girarla)"),
    ),
    (
        Msg::DesignAnchorCentreHover,
        t("Devolver el ancla al centro de la capa; la capa se queda donde está"),
    ),
    (Msg::DesignReasonCentred, t("el ancla ya está en el centro")),
    (Msg::DesignReadoutAngle, t("{free}°")),
    (Msg::DesignReadoutCrop, t("Recorte {edge} {pixels} px")),
    (Msg::DesignReadoutAnchor, t("Ancla {x}% · {y}%")),
    // User animation presets and shared preset files (GX-011, SW-019).
    (Msg::AnimMyPresets, t("Mis preajustes")),
    (
        Msg::AnimMyPresetsHover,
        t(
            "Preajustes guardados en este proyecto a partir de sus propias animaciones. Guarde las claves del elemento seleccionado en esta fase y elíjalas luego en el menú de preajustes de cualquier elemento",
        ),
    ),
    (Msg::AnimPresetNameHint, t("Nombre del preajuste")),
    (
        Msg::AnimPresetNameHover,
        t(
            "Nombre de un preajuste nuevo, o el nuevo nombre del elegido: único en el proyecto, como máximo 128 bytes",
        ),
    ),
    (Msg::AnimPresetSave, t("Guardar como preajuste…")),
    (
        Msg::AnimPresetSaveHover,
        t(
            "Guardar las claves de {phase} de {name} como un preajuste reutilizable en este proyecto. Posición, escala, rotación, opacidad y recorte quedan relativos a la pose de reposo del elemento, así que el preajuste sirve para cualquier elemento (una edición del proyecto que se puede deshacer)",
        ),
    ),
    (Msg::AnimPresetLibraryPick, t("Elegir un preajuste")),
    (
        Msg::AnimPresetLibraryPickHover,
        t("Elija uno de los preajustes del proyecto para renombrarlo, eliminarlo o exportarlo"),
    ),
    (Msg::AnimPresetLibraryItem, t("{name} · {phase}")),
    (
        Msg::AnimUserPresetHover,
        t(
            "{name}: su preajuste, guardado de la fase {phase} en {frames}. Se ajusta a la duración de esta fase conservando las proporciones de su ritmo",
        ),
    ),
    (Msg::AnimPresetRename, t("Renombrar")),
    (
        Msg::AnimPresetRenameHover,
        t(
            "Renombrar el preajuste elegido con el nombre escrito a la izquierda (las animaciones a las que se aplicó no cambian)",
        ),
    ),
    (Msg::AnimPresetDelete, t("Eliminar preajuste")),
    (
        Msg::AnimPresetDeleteHover,
        t(
            "Quitar el preajuste elegido del proyecto. Las animaciones a las que se aplicó conservan sus claves; Deshacer lo recupera",
        ),
    ),
    (Msg::AnimPresetPathHint, t("Ruta a un archivo .fmpreset")),
    (
        Msg::AnimPresetPathHover,
        t(
            "Ruta de un archivo .fmpreset en este equipo, para exportar el preajuste elegido o importar uno",
        ),
    ),
    (Msg::AnimPresetExport, t("Exportar preajuste")),
    (
        Msg::AnimPresetExportHover,
        t(
            "Escribir el preajuste elegido en el archivo .fmpreset de la ruta, para compartirlo con otros proyectos",
        ),
    ),
    (Msg::AnimPresetImport, t("Importar preajuste")),
    (
        Msg::AnimPresetImportHover,
        t(
            "Leer el archivo .fmpreset de la ruta y añadir su preajuste a este proyecto (con otro nombre si el suyo está ocupado)",
        ),
    ),
    (
        Msg::AnimReasonPresetName,
        t("escriba un nombre para el preajuste"),
    ),
    (
        Msg::AnimReasonPresetNameInvalid,
        t("el nombre supera los 128 bytes o tiene caracteres de control"),
    ),
    (
        Msg::AnimReasonPresetNameTaken,
        t("otro preajuste ya tiene este nombre"),
    ),
    (
        Msg::AnimReasonNoSaveTarget,
        t("seleccione una clave o una fila de propiedad del elemento que quiere guardar"),
    ),
    (
        Msg::AnimReasonNothingToSave,
        t("este elemento no tiene claves en esta fase"),
    ),
    (
        Msg::AnimReasonLibraryFull,
        t("el proyecto ya tiene 64 preajustes"),
    ),
    (
        Msg::AnimReasonNoPresetPicked,
        t("elija primero un preajuste"),
    ),
    (
        Msg::AnimReasonNoUserPresets,
        t("este proyecto aún no tiene preajustes propios"),
    ),
    (
        Msg::AnimReasonNoPath,
        t("escriba primero la ruta de un archivo"),
    ),
    (
        Msg::AnimReasonPresetsPending,
        t("la biblioteca de preajustes aún se está sincronizando"),
    ),
    (
        Msg::AnimPresetExported,
        t("Preajuste {name} exportado a {file}"),
    ),
    (
        Msg::AnimPresetImporting,
        t("Importando el preajuste {name} desde {file}"),
    ),
    (
        Msg::AnimPresetFileFailed,
        t("Archivo de preajuste {file}: {error}"),
    ),
    (Msg::ColorGradeFile, t("Archivo de corrección")),
    (
        Msg::ColorGradeFileHover,
        t(
            "Comparta correcciones entre proyectos como archivos .fmgrade: exporte la corrección que edita o importe una a la biblioteca de este proyecto",
        ),
    ),
    (Msg::ColorGradePathHint, t("Ruta a un archivo .fmgrade")),
    (
        Msg::ColorGradePathHover,
        t("Ruta de un archivo .fmgrade en este equipo, para exportar la corrección o importar una"),
    ),
    (Msg::ColorGradeExport, t("Exportar corrección")),
    (
        Msg::ColorGradeExportHover,
        t(
            "Escribir la corrección que edita en el archivo .fmgrade de la ruta, con su LUT dentro (o al lado como archivo .cube si la LUT es grande)",
        ),
    ),
    (Msg::ColorGradeImport, t("Importar corrección")),
    (
        Msg::ColorGradeImportHover,
        t(
            "Leer el archivo .fmgrade de la ruta y añadir su corrección, y su LUT, a la biblioteca de este proyecto (con otro nombre si alguno está ocupado)",
        ),
    ),
    (
        Msg::ColorGradeExported,
        t("Corrección {name} exportada a {file}"),
    ),
    (
        Msg::ColorGradeExportedLut,
        t("Corrección {name} exportada a {file}; su LUT está en {lut}"),
    ),
    (
        Msg::ColorGradeExportedLutMissing,
        t(
            "Corrección {name} exportada a {file}, pero su LUT no se pudo leer aquí: copie el archivo LUT del proyecto a {lut}",
        ),
    ),
    (
        Msg::ColorGradeImporting,
        t("Importando la corrección {name} desde {file}"),
    ),
    (
        Msg::ColorGradeFileFailed,
        t("Archivo de corrección {file}: {error}"),
    ),
    (
        Msg::ColorGradeLutTooLarge,
        t(
            "La LUT de {name} es demasiado grande para importarla por la conexión; importe la corrección con la línea de órdenes (freemix-cli grade-import)",
        ),
    ),
    (
        Msg::ColorGradeLutExists,
        t(
            "Exportar la corrección {name} reemplazaría el archivo LUT existente {lut}; reemplácelo o consérvelo",
        ),
    ),
    (
        Msg::ColorGradeLutExistsHover,
        t(
            "Una corrección cuya LUT es demasiado grande para incrustarla la escribe como archivo .cube junto al .fmgrade, y ya hay un archivo con ese nombre; puede ser otra LUT, así que no se escribió nada",
        ),
    ),
    (Msg::ColorGradeLutReplace, t("Reemplazar archivo LUT")),
    (
        Msg::ColorGradeLutReplaceHover,
        t(
            "Exportar de nuevo la corrección, sobrescribiendo el archivo .cube existente junto al .fmgrade con la LUT de esta corrección",
        ),
    ),
    (Msg::ColorGradeLutKeep, t("Conservar el archivo")),
    (
        Msg::ColorGradeLutKeepHover,
        t(
            "Cancelar esta exportación y dejar intacto el archivo .cube existente; elija otra ruta .fmgrade para exportar junto a él",
        ),
    ),
    (Msg::TabMedia, t("Medios")),
    (
        Msg::TabMediaHover,
        t(
            "Entradas de archivo: reproducir, desplazar, recortar, cambiar la velocidad y elegir la acción final y la pista de audio",
        ),
    ),
    (
        Msg::MediaIntro,
        t(
            "Cada entrada de archivo tiene su propio transporte. Reproducir, Pausa, Detener y buscar son controles de operador; el recorte, la velocidad, la acción final y la pista de audio se guardan con el proyecto.",
        ),
    ),
    (Msg::MediaNone, t("Sin entradas de archivo")),
    (
        Msg::MediaNoneDetail,
        t("Añada una entrada de archivo para reproducirla aquí."),
    ),
    (
        Msg::MediaNotSynced,
        t("Las entradas de archivo aparecen cuando el estado del proyecto está sincronizado."),
    ),
    (Msg::MediaStateWaiting, t("ESPERANDO")),
    (Msg::MediaStateEnded, t("TERMINADO")),
    (
        Msg::MediaStateHover,
        t("Estado del transporte de esta entrada"),
    ),
    (
        Msg::MediaLiveHover,
        t("El archivo aún se está escribiendo; la reproducción sigue su final creciente"),
    ),
    (Msg::MediaEofWaiting, t("ESPERANDO DATOS")),
    (
        Msg::MediaEofWaitingHover,
        t(
            "La reproducción alcanzó el final escrito hasta ahora y continúa en cuanto el archivo crece",
        ),
    ),
    (Msg::MediaOffline, t("SIN ARCHIVO")),
    (
        Msg::MediaOfflineHover,
        t("El archivo no se puede reproducir: {fault}"),
    ),
    (
        Msg::MediaPositionHover,
        t(
            "Cabezal / final del rango reproducible (código de tiempo de origen); bucles hasta ahora: {count}",
        ),
    ),
    (Msg::MediaPlay, t("Reproducir")),
    (
        Msg::MediaPlayHover,
        t("Reproducir esta entrada desde el cabezal"),
    ),
    (
        Msg::MediaPauseHover,
        t("Congelar el fotograma actual; Reproducir continúa desde él"),
    ),
    (
        Msg::MediaStopHover,
        t("Detener y volver al punto de entrada del recorte"),
    ),
    (Msg::MediaComplete, t("Finalizar")),
    (
        Msg::MediaCompleteHover,
        t(
            "Declarar terminado el archivo creciente, para que su último fotograma se reproduzca como final",
        ),
    ),
    (
        Msg::MediaScrubHover,
        t("Arrastre para buscar; el cabezal se sitúa en el fotograma exacto al soltar"),
    ),
    (Msg::MediaTrimIn, t("Entrada")),
    (
        Msg::MediaTrimInHover,
        t("Recorte de entrada: el primer fotograma que se reproduce"),
    ),
    (Msg::MediaTrimOut, t("Salida")),
    (
        Msg::MediaTrimOutHover,
        t(
            "Recorte de salida: la reproducción termina antes de este fotograma (el final del archivo cuando es igual al número de fotogramas)",
        ),
    ),
    (
        Msg::MediaSpeedHover,
        t("Velocidad de reproducción, de 0,25× a 4×"),
    ),
    (Msg::MediaSpeedOptionHover, t("Reproducir a {speed}")),
    (
        Msg::MediaEndActionHover,
        t("Qué ocurre al final del rango reproducible"),
    ),
    (Msg::MediaEndStop, t("Detener al final")),
    (
        Msg::MediaEndStopHover,
        t("Al final, detener y volver al punto de entrada"),
    ),
    (Msg::MediaEndHold, t("Congelar último fotograma")),
    (
        Msg::MediaEndHoldHover,
        t("Al final, seguir mostrando el último fotograma"),
    ),
    (
        Msg::MediaEndLoopHover,
        t("Al final, volver al punto de entrada y seguir reproduciendo"),
    ),
    (
        Msg::MediaAudioTrackHover,
        t("La pista de audio que reproduce esta entrada"),
    ),
    (Msg::MediaAudioDefault, t("Pista predeterminada")),
    (
        Msg::MediaAudioDefaultHover,
        t("Reproducir la pista de audio predeterminada del archivo"),
    ),
    (
        Msg::MediaAudioTrackOptionHover,
        t("Reproducir la pista de audio {track}"),
    ),
    (Msg::MediaNoAudio, t("Sin pistas de audio")),
    (
        Msg::MediaReasonUnavailable,
        t("el archivo de medios no está disponible"),
    ),
    (Msg::MediaLanguageEnglish, t("Inglés")),
    (Msg::MediaLanguageSpanish, t("Español")),
    (Msg::MediaLanguageFrench, t("Francés")),
    (Msg::MediaLanguageGerman, t("Alemán")),
    (Msg::MediaLanguageItalian, t("Italiano")),
    (Msg::MediaLanguagePortuguese, t("Portugués")),
    // Input lifecycle (IN-026).
    (Msg::LcDuplicate, t("Duplicar")),
    (
        Msg::LcDuplicateHover,
        t("Añadir una copia de {name} con su fuente, ajustes de audio, categoría y color"),
    ),
    (Msg::LcReplaceEllipsis, t("Reemplazar fuente…")),
    (
        Msg::LcReplaceHover,
        t("Cambiar lo que muestra esta entrada; conserva su nombre, posición y ajustes"),
    ),
    (Msg::LcCategory, t("Categoría")),
    (
        Msg::LcCategoryHover,
        t("Archivar esta entrada en una categoría del proyecto para el filtro del banco"),
    ),
    (Msg::LcCategoryNone, t("Sin categoría")),
    (
        Msg::LcCategoryNoneHover,
        t("Sacar esta entrada de su categoría"),
    ),
    (
        Msg::LcCategoryOptionHover,
        t("Archivar esta entrada en {category}"),
    ),
    (Msg::LcManageCategoriesEllipsis, t("Gestionar categorías…")),
    (
        Msg::LcManageCategoriesHover,
        t("Añadir, renombrar o quitar las categorías de entradas del proyecto"),
    ),
    (Msg::LcColour, t("Etiqueta de color")),
    (
        Msg::LcColourHover,
        t("Marcar el mosaico de esta entrada con una franja de color"),
    ),
    (Msg::LcColourNone, t("Sin color")),
    (
        Msg::LcColourNoneHover,
        t("Quitar la etiqueta de color de esta entrada"),
    ),
    (
        Msg::LcColourOptionHover,
        t("Etiquetar esta entrada en {colour}"),
    ),
    (Msg::LcColourOrange, t("Naranja")),
    (Msg::LcColourYellow, t("Amarillo")),
    (Msg::LcColourTeal, t("Turquesa")),
    (Msg::LcColourPurple, t("Morado")),
    (Msg::LcColourPink, t("Rosa")),
    (Msg::LcPause, t("Pausar entrada")),
    (
        Msg::LcPauseHover,
        t("Congelar el último fotograma de esta entrada y suspender su reproducción o captura"),
    ),
    (Msg::LcResume, t("Reanudar entrada")),
    (
        Msg::LcResumeHover,
        t("Reanudar la reproducción o captura de esta entrada"),
    ),
    (Msg::LcClose, t("Cerrar entrada")),
    (
        Msg::LcCloseHover,
        t(
            "Liberar el archivo, dispositivo o conexión de esta entrada hasta reabrirla; el mosaico sigue en el banco",
        ),
    ),
    (Msg::LcReopen, t("Reabrir entrada")),
    (
        Msg::LcReopenHover,
        t("Volver a abrir la fuente de esta entrada"),
    ),
    (Msg::LcBadgeOffline, t("SIN SEÑAL")),
    (Msg::LcBadgeClosed, t("CERRADA")),
    (Msg::LcOfflineDetail, t("{badge} ({reason})")),
    (Msg::LcFilterAll, t("Todas")),
    (
        Msg::LcFilterAllHover,
        t("Mostrar todas las entradas del banco"),
    ),
    (Msg::LcFilterUncategorised, t("Sin categoría")),
    (
        Msg::LcFilterUncategorisedHover,
        t("Mostrar solo las entradas sin categoría"),
    ),
    (
        Msg::LcFilterCategoryHover,
        t("Mostrar solo las entradas archivadas en {category}"),
    ),
    (
        Msg::LcFilterKeysNote,
        t("Las teclas numéricas siguen seleccionando por posición en el banco"),
    ),
    (Msg::LcFilterShowing, t("{shown} de {total} visibles")),
    (Msg::LcImportEllipsis, t("Importar archivos…")),
    (
        Msg::LcImportHover,
        t("Añadir una entrada de medios por archivo a partir de una lista de rutas"),
    ),
    (Msg::LcCategoriesEllipsis, t("Categorías…")),
    (Msg::LcImportTitle, t("Importar archivos")),
    (
        Msg::LcImportIntro,
        t(
            "Pegue una ruta de archivo por línea. Cada archivo se convierte en una entrada de medios con su nombre; el daemon copia los archivos del equipo al proyecto.",
        ),
    ),
    (
        Msg::LcImportPathsHover,
        t("Rutas de archivo, una por línea: rutas absolutas del equipo o URI asset://"),
    ),
    (
        Msg::LcImportCount,
        plural(
            "{count} archivo para importar",
            "{count} archivos para importar",
        ),
    ),
    (Msg::LcImportKey, t("IMPORTAR")),
    (
        Msg::LcImportKeyHover,
        t("Añadir una entrada de medios por cada archivo de la lista"),
    ),
    (Msg::LcImportCancelHover, t("Cerrar sin importar")),
    (
        Msg::LcReasonNoPaths,
        t("no hay rutas de archivo para importar"),
    ),
    (
        Msg::LcReasonTooManyItems,
        t("una importación admite como máximo 64 archivos"),
    ),
    (
        Msg::LcReasonImportOverLimit,
        t("la importación superaría el máximo de 256 entradas"),
    ),
    (
        Msg::LcReasonPathTooLong,
        t("una ruta supera los 1024 bytes"),
    ),
    (
        Msg::LcReasonNoFreeIds,
        t("no queda ningún identificador de entrada libre"),
    ),
    (Msg::LcReplaceTitle, t("Reemplazar fuente")),
    (
        Msg::LcReplaceIntro,
        t(
            "{name} conserva su nombre, posición en el banco, ajustes de audio y etiquetas; solo cambia su fuente.",
        ),
    ),
    (Msg::LcSourceSolid, t("SÓLIDO")),
    (
        Msg::LcSourceSolidHover,
        t("Reemplazar por un generador de color sólido"),
    ),
    (Msg::LcSourceBars, t("BARRAS")),
    (Msg::LcSourceBarsHover, t("Reemplazar por barras de color")),
    (Msg::LcSourceMedia, t("ARCHIVO")),
    (
        Msg::LcSourceMediaHover,
        t("Reproducir un archivo de medios; una ruta del equipo se copia al proyecto"),
    ),
    (Msg::LcSourceDevice, t("DISPOSITIVO")),
    (
        Msg::LcSourceDeviceHover,
        t("Capturar de un dispositivo por su clave estable"),
    ),
    (Msg::LcSourceNetwork, t("RED")),
    (
        Msg::LcSourceNetworkHover,
        t("Recibir una señal de red desde un punto de conexión"),
    ),
    (
        Msg::LcSourceMediaFieldHover,
        t("Ruta absoluta de archivo del equipo o URI asset://"),
    ),
    (
        Msg::LcSourceDeviceFieldHover,
        t("La clave estable del dispositivo, tal como la lista el daemon"),
    ),
    (
        Msg::LcSourceNetworkFieldHover,
        t("Punto de conexión de red, como srt://host:9000"),
    ),
    (Msg::LcReplaceKey, t("REEMPLAZAR")),
    (
        Msg::LcReplaceKeyHover,
        t("Cambiar esta entrada a la nueva fuente"),
    ),
    (Msg::LcReplaceCancelHover, t("Cerrar sin cambiar la fuente")),
    (
        Msg::LcReasonSourceEmpty,
        t("el campo de la fuente está vacío"),
    ),
    (
        Msg::LcReasonSourceTooLong,
        t("la fuente supera los 1024 bytes"),
    ),
    (Msg::LcCategoriesTitle, t("Categorías de entradas")),
    (
        Msg::LcCategoriesIntro,
        t(
            "Las categorías agrupan entradas para el filtro del banco. Un cambio de nombre se aplica a todas las entradas de la categoría.",
        ),
    ),
    (Msg::LcCategoriesEmpty, t("Aún no hay categorías.")),
    (
        Msg::LcCategoryNameHover,
        t("Nombre de la categoría: hasta 48 bytes, único"),
    ),
    (Msg::LcCategoryRemove, t("Eliminar")),
    (
        Msg::LcCategoryRemoveHover,
        t("Quitar esta categoría de la lista"),
    ),
    (
        Msg::LcCategoryNewHover,
        t("Nombre para una nueva categoría"),
    ),
    (Msg::LcCategoryAdd, t("Añadir categoría")),
    (
        Msg::LcCategoryAddHover,
        t("Añadir el nombre escrito como nueva categoría"),
    ),
    (
        Msg::LcCategoriesApplyHover,
        t("Guardar la lista de categorías en el proyecto"),
    ),
    (
        Msg::LcCategoriesCancelHover,
        t("Cerrar sin guardar los cambios de categorías"),
    ),
    (
        Msg::LcReasonCategoryInUse,
        t("aún hay entradas archivadas en esta categoría"),
    ),
    (
        Msg::LcReasonCategoryLimit,
        t("el proyecto ya tiene 32 categorías"),
    ),
    (
        Msg::LcReasonCategoryNameTooLong,
        t("el nombre supera los 48 bytes"),
    ),
    (
        Msg::LcReasonCategoryTaken,
        t("otra categoría ya usa este nombre"),
    ),
    (
        Msg::LcReasonCategoriesUnchanged,
        t("la lista de categorías no ha cambiado"),
    ),
];
