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
        t("el show no tiene destinos de stream configurados"),
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
        t("Un destino de stream informa que está en vivo"),
    ),
    (
        Msg::LiveHoverDesired,
        t("Un destino de stream debe estar activo pero no informa que esté en vivo"),
    ),
    (
        Msg::LiveHoverIdle,
        t("Ningún destino de stream debe estar activo"),
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
        t("1–8 PVW   C CORTE   F FUNDIDO   B FTB   ESC CANCELAR T-BAR   ·   AYUDA › ATAJOS"),
    ),
    (Msg::StatusBarKeys, t("TECLAS")),
    // Toasts.
    (Msg::ToastNotice, t("AVISO")),
    (Msg::ToastError, t("ERROR")),
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
        t("Muestra u oculta grabación, streams, overlays y stingers"),
    ),
    (Msg::ViewStatusBar, t("Barra de estado")),
    (
        Msg::ViewStatusBarHover,
        t("Muestra u oculta la barra de estado inferior"),
    ),
    (Msg::ViewSidePanelTab, t("Pestaña del panel lateral")),
    (Msg::TabOverlays, t("Overlays")),
    (Msg::TabStingers, t("Stingers")),
    (Msg::ViewResetLayout, t("Restablecer diseño")),
    (
        Msg::ViewResetLayoutDisabled,
        t("Todos los paneles ya están visibles"),
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
    (Msg::StreamsHeading, t("Streams")),
    (Msg::NoStreamTargets, t("Sin destinos de stream")),
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
        t("Menú de la entrada: Previo, Corte, Renombrar, Mover, Quitar"),
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
        t("Más: Previo, Corte, Renombrar, Mover, Quitar"),
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
            "Render simulado solo de la fuente realizada; los overlays, las escenas y el fundido a negro no se componen en esta imagen.",
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
            "Los segmentos encendidos muestran el RMS; la línea fina es el último pico; la marca brillante retiene el pico 1,5 s y luego cae 20 dB/s; la lámpara superior se enclava a 0 dBFS hasta que se pulsa",
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
        t("Balance estéreo: L100 todo a la izquierda, C centro, R100 todo a la derecha"),
    ),
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
        t("TOMAR usa la fuente actual de Previo. D = pedido, C = última confirmación del daemon."),
    ),
    (
        Msg::OverlaysIntroHover,
        t("La confirmación es estado de control, no prueba del vídeo de salida."),
    ),
    (Msg::OverlayOnAir, t("AL AIRE")),
    (Msg::OverlayTaking, t("ENTRANDO")),
    (Msg::OverlayClearing, t("SALIENDO")),
    (Msg::TakeKey, t("TOMAR")),
    (Msg::UpdateKey, t("ACTUALIZAR")),
    (Msg::NextKey, t("SIGUIENTE")),
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
        t("Alternar este canal de overlay entre corte y fundido"),
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
        t("No hay destinos de stream configurados"),
    ),
    (
        Msg::StreamsNotSynced,
        t("Estado de streams sin sincronizar"),
    ),
    (
        Msg::StreamRowHover,
        t("{name}\nPedido {desired} · realizado {realized}"),
    ),
    (Msg::StartKey, t("INICIAR")),
    (Msg::StopKey, t("DETENER")),
    (
        Msg::StartStreamTarget,
        t("Iniciar el destino de stream «{name}»"),
    ),
    (
        Msg::StopStreamTarget,
        t("Detener el destino de stream «{name}»"),
    ),
    (
        Msg::ReasonStreamPending,
        t("un comando anterior para este destino de stream sigue pendiente"),
    ),
];
