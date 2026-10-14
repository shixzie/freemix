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
    (Msg::TransitionWipe, t("Cortinilla")),
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
];
