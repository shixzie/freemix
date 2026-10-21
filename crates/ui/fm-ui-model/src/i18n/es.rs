//! Spanish catalog.
//!
//! Broadcast abbreviations that Spanish-speaking operators read as-is on
//! hardware (`PGM`, `PVW`, `REC`, `T-bar`, `Stinger`, `FIFO`, and the
//! audio terms `SIP`, `PFL`, `AFL`, `MON`, `CLIP`) stay untranslated, and
//! their hover help says what they mean in Spanish; full words are translated (`Previo`, `Programa`, `Corte`).
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
        Msg::ReasonTransitionInFlight,
        t("hay una transición en curso; espera a que termine"),
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
        t(
            "no hay ninguna edición del proyecto que deshacer; solo se pueden deshacer las ediciones hechas desde que el servidor abrió el show",
        ),
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
    (Msg::HealthPreviewFeedLive, t("SEÑAL PVW")),
    (Msg::HealthPreviewFeedStale, t("SEÑAL PVW {age}")),
    (Msg::HealthPreviewFeedNone, t("SIN SEÑAL PVW")),
    (
        Msg::HealthPreviewFeedHover,
        t(
            "Monitor de Previo: fotograma de {width}×{height} recibido hace {age}. Pasa a ámbar tras 1 s sin un fotograma nuevo.",
        ),
    ),
    (
        Msg::HealthPreviewFeedNoneHover,
        t("No ha llegado ningún fotograma del monitor de Previo desde el daemon"),
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
    // Daemon utilization and alerts (RC-015).
    (Msg::HealthCpu, t("CPU {value}")),
    (Msg::HealthCpuUnavailable, t("CPU —")),
    (
        Msg::HealthCpuHover,
        t(
            "CPU del proceso del daemon: {value} de todas las CPU lógicas en el último segundo. Ámbar desde el 75 %; rojo mientras salta la alerta de CPU (90 % durante 10 s).",
        ),
    ),
    (
        Msg::HealthCpuUnavailableHover,
        t("El daemon aún no informa de la CPU, o no puede en esta plataforma"),
    ),
    (Msg::HealthGpu, t("GPU {value}")),
    (Msg::HealthGpuUnavailable, t("GPU —")),
    (
        Msg::HealthGpuHover,
        t(
            "Estimación de GPU: {value} del último segundo en las pasadas de render cronometradas del compositor, no un contador del fabricante. Ámbar desde el 75 %; rojo mientras salta la alerta de GPU.",
        ),
    ),
    (
        Msg::HealthGpuUnavailableHover,
        t(
            "Sin estimación de GPU: el daemon simulado no renderiza en GPU y algunos adaptadores no cronometran las pasadas",
        ),
    ),
    (
        Msg::HealthAlerts,
        plural("{count} ALERTA", "{count} ALERTAS"),
    ),
    (Msg::HealthAlertsNone, t("SIN ALERTAS")),
    (
        Msg::HealthAlertsHover,
        t("Alertas del daemon activas ahora:"),
    ),
    (
        Msg::HealthAlertsNoneHover,
        t(
            "No hay ninguna alerta del daemon activa. Vigila CPU, GPU, fotogramas tardíos, imágenes descartadas, espacio en disco de las salidas y salidas fallidas.",
        ),
    ),
    (Msg::AlertLine, t("{rule} ({severity}) desde hace {age}")),
    (
        Msg::AlertLastNotification,
        t("Última: {rule} {kind} hace {age}"),
    ),
    (Msg::AlertRuleCpuSaturated, t("CPU saturada")),
    (Msg::AlertRuleDiskLow, t("poco disco en las salidas")),
    (Msg::AlertRuleFrameLate, t("fotogramas tardíos")),
    (Msg::AlertRuleFramesDropped, t("imágenes descartadas")),
    (Msg::AlertRuleGpuSaturated, t("GPU saturada")),
    (Msg::AlertRuleOutputFailed, t("salida fallida")),
    (Msg::AlertSeverityInfo, t("información")),
    (Msg::AlertSeverityWarning, t("aviso")),
    (Msg::AlertSeverityCritical, t("crítica")),
    (Msg::AlertKindFiring, t("saltó")),
    (Msg::AlertKindResolved, t("se resolvió")),
    (Msg::AlertKindReminder, t("sigue activa")),
    // Live source clock drift correction (AU-004).
    (Msg::HealthSync, t("SINC {drift}")),
    (
        Msg::HealthSyncFaults,
        plural("SINC {count} FALLO", "SINC {count} FALLOS"),
    ),
    (
        Msg::HealthSyncHover,
        t(
            "Relojes de fuentes en vivo: {sources} con deriva corregida al reloj del motor · deriva máxima {drift} ppm · {faults} resincronizaciones, vaciados o desbordes. Ámbar mientras una fuente se llena o se recupera, o cuando alguna ha tenido un fallo.",
        ),
    ),
    (Msg::SyncKindCapture, t("captura")),
    (Msg::SyncKindStream, t("emisión de red")),
    (Msg::SyncKindSystemAudio, t("audio del sistema")),
    (Msg::SyncKindScreenCapture, t("captura de pantalla")),
    (Msg::SyncStatePriming, t("llenando")),
    (Msg::SyncStateRunning, t("en marcha")),
    (Msg::SyncStateRecovering, t("recuperándose")),
    (
        Msg::SyncBadgeHover,
        t(
            "Corrección de deriva de reloj de {name} ({kind}, {state}): su audio se remuestrea al reloj del motor y sus imágenes lo siguen.\nHaz clic para ver la relación, el búfer y los recuentos de recuperación.",
        ),
    ),
    (
        Msg::SyncSourceLine,
        t(
            "{name}: deriva {drift} ppm · corrección {correction} ppm · relación {ratio} · búfer {buffer} de {target} ms",
        ),
    ),
    (
        Msg::SyncDetailDrift,
        t("Deriva {drift} ppm, corrección {correction} ppm"),
    ),
    (
        Msg::SyncDetailRatio,
        t("Relación de remuestreo {ratio} ({input} Hz a {output} Hz)"),
    ),
    (
        Msg::SyncDetailBuffer,
        t("Búfer {buffer} ms, objetivo {target} ms"),
    ),
    (
        Msg::SyncDetailFaults,
        t("{resyncs} resincronizaciones · {underruns} vaciados · {overruns} desbordes"),
    ),
    (
        Msg::SyncDetailPictures,
        t("{drops} imágenes descartadas · {repeats} repetidas · reconexiones: {restarts}"),
    ),
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
    (Msg::ViewStatusBar, t("Barra de estado")),
    (
        Msg::ViewStatusBarHover,
        t("Muestra u oculta la barra de estado inferior"),
    ),
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
        t(
            "Vuelve al diseño predeterminado: todos los paneles acoplados, sin ventanas propias y tamaños predeterminados",
        ),
    ),
    (
        Msg::ViewLayoutRemembered,
        t(
            "El diseño, las ventanas desacopladas y los diseños guardados se recuerdan en esta consola; arrastre una pestaña para mover un panel y los huecos para cambiar tamaños",
        ),
    ),
    (Msg::PanelPreview, t("Previo")),
    (
        Msg::PanelPreviewHover,
        t("El monitor de Previo: la fuente que la próxima transición lleva al aire"),
    ),
    (Msg::PanelProgram, t("Programa")),
    (
        Msg::PanelProgramHover,
        t("El monitor de Programa: lo que está al aire"),
    ),
    (
        Msg::PanelTransitionsHover,
        t("Teclas de transición, la T-bar manual, las favoritas y el fundido a negro"),
    ),
    (Msg::PanelOutputs, t("Grabación y transmisiones")),
    (
        Msg::PanelOutputsHover,
        t("Grabadores y destinos de transmisión: iniciar, detener y su estado en vivo"),
    ),
    (
        Msg::ToolColorHover,
        t("Corrige el color de entradas y Programa con ruedas, curvas y LUT"),
    ),
    (
        Msg::ToolAnimationHover,
        t("Anima títulos y escenas con fotogramas clave en una línea de tiempo"),
    ),
    (
        Msg::ToolDesignerHover,
        t("Organiza las capas de una escena sobre su lienzo"),
    ),
    (
        Msg::DockEmpty,
        t(
            "Todos los paneles están ocultos o en su propia ventana. Vuelva a mostrarlos desde Ver › Paneles o Ver › Diseños.",
        ),
    ),
    (
        Msg::DockTabHover,
        t(
            "Clic para mostrar este panel. Arrastre la pestaña al centro de otro panel para agruparlos, o a un borde para dividirlo; clic derecho para más.",
        ),
    ),
    (Msg::DockPopOut, t("Desacoplar")),
    (
        Msg::DockPopOutHover,
        t("Mueve este panel a su propia ventana; cierre esa ventana para volver a acoplarlo"),
    ),
    (
        Msg::DockPopOutNamed,
        t("Mueve {panel} a su propia ventana; cierre esa ventana para volver a acoplarlo"),
    ),
    (Msg::DockClosePanel, t("Cerrar panel")),
    (
        Msg::DockClosePanelHover,
        t("Oculta este panel; vuelva a abrirlo desde Ver › Paneles"),
    ),
    (
        Msg::DockClosePanelNamed,
        t("Oculta {panel}; vuelva a abrirlo desde Ver › Paneles"),
    ),
    (Msg::DockBack, t("Acoplar de nuevo")),
    (
        Msg::DockBackNamed,
        t("Devuelve {panel} a la consola; cerrar esta ventana hace lo mismo"),
    ),
    (Msg::DockSplitter, t("Divisor de paneles")),
    (
        Msg::DockSplitterHover,
        t("Arrastre para cambiar el tamaño de los paneles a ambos lados"),
    ),
    (Msg::DockGrip, t("Mover panel")),
    (
        Msg::DockGripHover,
        t(
            "Arrastre para mover; suéltelo en un borde para dividir, o en el centro para unir sus pestañas",
        ),
    ),
    (
        Msg::DockTabMaximizeHover,
        t("Doble clic para maximizar este panel; doble clic de nuevo o Esc para restaurarlo."),
    ),
    (Msg::DockRestore, t("Restaurar paneles")),
    (
        Msg::DockRestoreHover,
        t("Vuelve a mostrar todos los paneles  [Esc]"),
    ),
    (Msg::DockMoreTabs, t("Más pestañas")),
    (
        Msg::DockMoreTabsHover,
        t("Paneles de este grupo sin sitio para su pestaña: elija uno para mostrarlo"),
    ),
    (Msg::DockPopoutGrip, t("Arrastrar para acoplar")),
    (
        Msg::DockPopoutGripHover,
        t(
            "Arrastre sobre la ventana de la consola para acoplar {panel} donde indica la vista previa azul; Acoplar en… ofrece los mismos lugares",
        ),
    ),
    (Msg::DockBackTo, t("Acoplar en…")),
    (
        Msg::DockBackToHover,
        t(
            "Elija dónde se acopla {panel} en la consola: junto a un panel o en su grupo de pestañas",
        ),
    ),
    (
        Msg::DockBackToNone,
        t("No hay paneles acoplados; Acoplar de nuevo devuelve este a su sitio"),
    ),
    (
        Msg::DockTargetHover,
        t(
            "Acopla junto a {panels} o en su grupo; pase el puntero por un lugar para verlo en la consola",
        ),
    ),
    (Msg::DockZoneCenter, t("En su grupo de pestañas")),
    (Msg::DockZoneLeft, t("A su izquierda")),
    (Msg::DockZoneRight, t("A su derecha")),
    (Msg::DockZoneTop, t("Encima")),
    (Msg::DockZoneBottom, t("Debajo")),
    (
        Msg::DockZoneHover,
        t("Acopla {panel} aquí; la consola muestra dónde quedará"),
    ),
    (
        Msg::LayoutRejected,
        t("No se pudo leer el diseño de paneles guardado; se muestra el diseño predeterminado"),
    ),
    (Msg::LayoutDefault, t("Predeterminado")),
    (
        Msg::LayoutDefaultHover,
        t(
            "Monitores y transiciones arriba, el banco de entradas y el mezclador debajo, salidas y pestañas laterales a la derecha",
        ),
    ),
    (Msg::LayoutCompact, t("Compacto")),
    (
        Msg::LayoutCompactHover,
        t(
            "Menos paneles y más grandes para pantallas pequeñas: el banco y el mezclador comparten pestañas, igual que las salidas y las pestañas laterales",
        ),
    ),
    (Msg::LayoutAudioFocus, t("Enfoque en audio")),
    (
        Msg::LayoutAudioFocusHover,
        t(
            "Un mezclador de audio ancho y alto junto a un grupo de pestañas con el banco, las salidas y las pestañas laterales",
        ),
    ),
    (
        Msg::LayoutNameTooLong,
        t("el nombre supera los 40 caracteres"),
    ),
    (
        Msg::LayoutNameBadCharacter,
        t("el nombre no puede contener | ni caracteres de control"),
    ),
    (
        Msg::LayoutNameTaken,
        t("otro diseño guardado ya usa este nombre"),
    ),
    (
        Msg::LayoutTooMany,
        t("ya hay 16 diseños guardados; elimine uno primero"),
    ),
    (Msg::LayoutSaveTitle, t("Guardar diseño")),
    (Msg::LayoutRenameTitle, t("Renombrar diseño")),
    (Msg::LayoutNameLabel, t("Nombre del diseño")),
    (
        Msg::LayoutNameHover,
        t("Un nombre para este diseño: hasta 40 caracteres, único"),
    ),
    (Msg::LayoutNameConfirm, t("GUARDAR")),
    (
        Msg::LayoutNameConfirmHover,
        t("Guarda el diseño con este nombre  [Intro]"),
    ),
    (Msg::LayoutNameCancelHover, t("Cerrar sin guardar  [Esc]")),
    (Msg::ViewLayouts, t("Diseños")),
    (
        Msg::ViewLayoutsHover,
        t("Cambia a una disposición de paneles incorporada o guardada"),
    ),
    (Msg::LayoutBuiltInHover, t("Diseño incorporado: {what}")),
    (Msg::LayoutSavedHeading, t("Diseños guardados")),
    (Msg::LayoutSavedNone, t("Aún no hay diseños guardados")),
    (
        Msg::LayoutSavedEntryHover,
        t("Diseño guardado: aplicarlo, renombrarlo o eliminarlo"),
    ),
    (
        Msg::LayoutApplyHover,
        t("Cambia a este diseño guardado, con sus paneles desacoplados"),
    ),
    (
        Msg::LayoutRenameHover,
        t("Da otro nombre a este diseño guardado"),
    ),
    (
        Msg::LayoutDeleteHover,
        t("Olvida este diseño guardado; la disposición actual no cambia"),
    ),
    (Msg::LayoutSaveCurrent, t("Guardar diseño actual…")),
    (
        Msg::LayoutSaveCurrentHover,
        t("Guarda la disposición actual, con los paneles desacoplados, con un nombre"),
    ),
    (
        Msg::ViewPanelToggleHover,
        t("Muestra u oculta este panel: {what}"),
    ),
    (
        Msg::ViewPanelPoppedHover,
        t(
            "En su propia ventana: {what}. Desmarca para ocultarlo; Traer al frente muestra su ventana delante",
        ),
    ),
    (Msg::ViewPanelFront, t("Traer al frente")),
    (
        Msg::ViewPanelFrontHover,
        t("{panel} está en su propia ventana: trae esa ventana al frente"),
    ),
    (Msg::ViewToolWindows, t("Ventanas propias")),
    (
        Msg::ViewToolWindowHover,
        t("Abre {tool} en su propia ventana en lugar de sobre la consola"),
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
        t(
            "Añade al final del banco una entrada de color sólido, barras de color, flujo de red o cámara",
        ),
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
    (Msg::TransitionVerticalWipe, t("Cortina vertical")),
    (Msg::TransitionVerticalSlide, t("Deslizar vertical")),
    (Msg::TransitionFly, t("Vuelo")),
    (Msg::TransitionFlyTurn, t("Vuelo con giro")),
    (Msg::TransitionCube, t("Cubo")),
    (Msg::TransitionCubeZoom, t("Cubo con zoom")),
    (Msg::TransitionCrossZoom, t("Zoom cruzado")),
    (Msg::TransitionMerge, t("Fusión")),
    (Msg::TransitionStinger, t("Stinger S{slot}")),
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
        Msg::ShortcutStartTBar,
        t("Iniciar la transición manual con T-bar seleccionada"),
    ),
    (
        Msg::ShortcutStepTBar,
        t("Mover la T-bar activa un 10 % hacia el final, o hacia atrás"),
    ),
    (
        Msg::ShortcutCommitManual,
        t("Completar la transición manual con T-bar activa"),
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
    (Msg::ShortcutGroupWindows, t("Ventanas")),
    (Msg::ShortcutGroupOutput, t("Grabación y emisión")),
    (Msg::ShortcutScripts, t("Abrir o cerrar la ventana Guiones")),
    (
        Msg::ShortcutLiveLan,
        t("Abrir o cerrar la ventana Espectadores LiveLAN"),
    ),
    (
        Msg::ShortcutRecord,
        t("Iniciar o detener la grabadora REC 1"),
    ),
    (
        Msg::ShortcutStartStreams,
        t("Iniciar todos los destinos de emisión detenidos"),
    ),
    (
        Msg::ShortcutStopStreams,
        t("Detener todos los destinos de emisión en marcha"),
    ),
    (Msg::MenuStartAllStreams, t("Iniciar todas las emisiones")),
    (Msg::MenuStopAllStreams, t("Detener todas las emisiones")),
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
    (Msg::RowRequestedNoSample, t("Iniciado · sin datos")),
    (Msg::RowDesiredOff, t("Sin iniciar")),
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
        t("Miniatura: ninguna (esta entrada aún no tiene imagen)"),
    ),
    (
        Msg::ThumbnailFeed,
        t("Miniatura: señal propia en vivo de esta entrada (una imagen por segundo, aprox.)"),
    ),
    (
        Msg::ThumbnailFeedStale,
        t("Miniatura: señal propia de esta entrada, desactualizada desde hace {age}"),
    ),
    (Msg::ThumbFeed, t("SEÑAL")),
    (Msg::ThumbFeedSim, t("SEÑAL · SIM")),
    (Msg::ThumbStale, t("OBSOLETA {age}")),
    (Msg::ThumbAudioOnly, t("solo audio")),
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
        t(
            "Añadir entrada…\nUna cámara, un flujo de red, una captura de pantalla o ventana, audio, una imagen, un documento o una lista, al final del banco",
        ),
    ),
    (Msg::AddingName, t("AÑADIENDO {name}…")),
    (
        Msg::InputCountHover,
        t("Entradas del show / el máximo que admite un show"),
    ),
    (
        Msg::AddTileKinds,
        t("cámara · red · pantalla · audio · imagen · lista"),
    ),
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
    (Msg::KeyArrows, t("Flechas arriba/abajo")),
    (Msg::KeyMoveUp, t("Subir")),
    (Msg::KeyMoveDown, t("Bajar")),
    (Msg::KeyRemove, t("Quitar de la lista")),
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
    (Msg::SlideKey, t("DESLIZAR")),
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
    (Msg::VerticalSlideKey, t("DESLIZAR V")),
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
            "T-bar manual: arrastre la palanca (abajo = inicio, arriba = fin) o muévala un 10 % con PgUp / PgDn, y luego {commit} [End] o {cancel} [Esc]",
        ),
    ),
    (
        Msg::TBarHoverIdle,
        t("T-bar manual: inicie una transición manual ({keys}) para habilitar la palanca"),
    ),
    (
        Msg::TBarHoverReady,
        t(
            "T-bar manual: suba la palanca para iniciar una transición reversible ({kind}) y recorrerla (abajo = inicio, arriba = fin); elija el tipo con una tecla MANUAL, MÁS o el menú de una favorita",
        ),
    ),
    (
        Msg::ManualMoreHover,
        t(
            "Iniciar otra transición manual: cualquier otra mezcla, o un Stinger configurado cuyo clip dura lo mismo que la duración compartida ({frames}) y sigue a la palanca",
        ),
    ),
    (Msg::FavoriteTBar, t("Iniciar en la T-bar")),
    (
        Msg::FavoriteTBarHover,
        t("Iniciar {effect} (favorita {slot}) como transición manual reversible en la T-bar"),
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
        t("SIN TOMA DE PREVIO · esperando el fotograma nativo de Previo"),
    ),
    (
        Msg::MonitorNoPreviewTapNote,
        t(
            "Aún no ha llegado ningún fotograma nativo de Previo; este monitor muestra el estado de enrutamiento.",
        ),
    ),
    (
        Msg::MonitorMetersProgramHover,
        t(
            "Audio de Programa: la mezcla máster, izquierda y derecha, en una escala de 60 dB (barras RMS, marcas pico)",
        ),
    ),
    (
        Msg::MonitorMetersPreviewHover,
        t(
            "Audio de Previo: el nivel propio de la fuente en previo, izquierda y derecha, en una escala de 60 dB (barras RMS, marcas pico)",
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
    (
        Msg::GroupSoloOnHover,
        t(
            "Grupo en solo: todos sus canales, también los de los grupos anidados en él, están en solo; haga clic para liberarlo",
        ),
    ),
    (
        Msg::GroupSoloOffHover,
        t("Poner en solo todos los canales de este grupo y de los grupos anidados en él"),
    ),
    (Msg::GroupParentNone, t("Nivel superior")),
    (Msg::GroupParentIn, t("En {group}")),
    (
        Msg::GroupParentHover,
        t(
            "Anidar este grupo dentro de otro: el fader, el silencio y el solo del grupo padre también se aplican a cada canal miembro",
        ),
    ),
    (
        Msg::GroupParentItemHover,
        t("Anidar este grupo dentro de {group}"),
    ),
    (
        Msg::GroupParentNoneHover,
        t("Sacar este grupo de su grupo padre"),
    ),
    (Msg::MonitorStrip, t("MON")),
    (Msg::MonitorFollowsMaster, t("= MÁSTER")),
    (Msg::MonitorSoloListen, t("ESCUCHA SOLO")),
    (
        Msg::MonitorStripHover,
        t(
            "Bus de monitoreo para auriculares: sigue al Máster hasta que un canal o grupo se pone en solo en modo PFL o AFL, y entonces lleva solo los canales en solo; el Programa nunca cambia",
        ),
    ),
    (Msg::MonitorNoDevice, t("SOLO MEDIDOR")),
    (
        Msg::MonitorNoDeviceHover,
        t(
            "No hay un dispositivo de auriculares elegido, así que el bus de monitoreo solo se mide; escribe el nombre de un dispositivo en el campo de arriba para escucharlo",
        ),
    ),
    (Msg::MonitorDevicePlaceholder, t("Sin dispositivo")),
    (
        Msg::MonitorDeviceHover,
        t(
            "Dispositivo de auriculares para el bus de monitoreo: un nombre de salida ALSA como default o hw:CARD=Headset,DEV=0. Pulsa Intro para reproducir el bus de monitoreo en él, o vacía el campo para detenerlo; el Programa nunca cambia",
        ),
    ),
    (
        Msg::MonitorDeviceShownHover,
        t(
            "Dispositivo de auriculares: {device}. Haz clic para cambiarlo, o vacíalo para detenerlo; el Programa nunca cambia",
        ),
    ),
    (Msg::MonitorOutputPlaying, t("AURIC. ACTIVOS")),
    (
        Msg::MonitorOutputPlayingHover,
        t("El bus de monitoreo se está reproduciendo en {device}"),
    ),
    (Msg::MonitorOutputFailed, t("AURIC. FALLÓ")),
    (
        Msg::MonitorOutputFailedHover,
        t(
            "No se pudo abrir el dispositivo {device} o dejó de reproducir; revisa el nombre y pulsa Intro en el campo del dispositivo para reintentar",
        ),
    ),
    (Msg::MonitorOutputIdle, t("AURIC. EN ESPERA")),
    (
        Msg::MonitorOutputIdleHover,
        t("Este servidor no está generando audio ahora, así que nada llega a {device}"),
    ),
    (
        Msg::MonitorLevelHover,
        t(
            "Nivel de auriculares: el volumen propio de la señal de monitoreo; el Programa, las grabaciones, las transmisiones y el medidor de monitoreo no cambian",
        ),
    ),
    (
        Msg::MonitorMuteOnHover,
        t("Auriculares silenciados: haz clic para volver a oír el bus de monitoreo"),
    ),
    (
        Msg::MonitorMuteOffHover,
        t("Silenciar solo la señal de auriculares; el Programa sigue sonando"),
    ),
    (Msg::SoloModeSip, t("SIP")),
    (Msg::SoloModePfl, t("PFL")),
    (Msg::SoloModeAfl, t("AFL")),
    (
        Msg::SoloModeHover,
        t("Modo de solo: cómo se escuchan las teclas S"),
    ),
    (
        Msg::SoloModeSipHover,
        t("Solo en sitio: poner en solo silencia todos los demás canales en el Programa"),
    ),
    (
        Msg::SoloModePflHover,
        t(
            "Escucha prefader: los canales en solo van al bus de monitoreo antes de su fader; el Programa no cambia",
        ),
    ),
    (
        Msg::SoloModeAflHover,
        t(
            "Escucha posfader: los canales en solo van al bus de monitoreo después de su fader; el Programa no cambia",
        ),
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
    (Msg::FtbOwnDuration, t("Duración propia")),
    (
        Msg::FtbOwnDurationOnHover,
        t(
            "{output} se funde con su propia duración; desmárquela para seguir la duración del fundido a negro",
        ),
    ),
    (
        Msg::FtbOwnDurationOffHover,
        t("{output} se funde con la duración del fundido a negro; márquela para darle una propia"),
    ),
    (
        Msg::FtbOutputDurationHover,
        t("Duración propia del fundido a negro de {output} en fotogramas (arrastre o escriba)"),
    ),
    (Msg::FtbCurveLabel, t("Curva")),
    (Msg::FtbCurveEase, t("Suave")),
    (
        Msg::FtbCurveLinearHover,
        t("El fundido a negro avanza el mismo paso en cada fotograma"),
    ),
    (
        Msg::FtbCurveEaseHover,
        t("El fundido a negro empieza y termina despacio (smoothstep)"),
    ),
    (
        Msg::MenuFullscreenOutputs,
        t("Salidas a pantalla completa…"),
    ),
    (
        Msg::MenuFullscreenOutputsHover,
        t(
            "Elija hasta dos pantallas que muestren Programa, Vista previa o una salida a pantalla completa, con modo, escalado y HDR",
        ),
    ),
    (Msg::DisplayOutputsTitle, t("Salidas a pantalla completa")),
    (
        Msg::DisplayOutputsIntro,
        t(
            "Cada ranura muestra una fuente a pantalla completa en una pantalla. Las ranuras se guardan con el proyecto y vuelven cuando se reconecta una pantalla.",
        ),
    ),
    (
        Msg::DisplayInventoryCount,
        plural("{count} pantalla conectada", "{count} pantallas conectadas"),
    ),
    (
        Msg::DisplayInventoryNoPresenter,
        t(
            "Este servidor no abre ventanas a pantalla completa; las ranuras se guardan y esperan a un presentador",
        ),
    ),
    (Msg::DisplayInventoryUnknown, t("Pantallas aún sin listar")),
    (
        Msg::DisplayInventoryError,
        t("La lista de pantallas falló: {error}"),
    ),
    (Msg::DisplayRefresh, t("Actualizar pantallas")),
    (
        Msg::DisplayRefreshHover,
        t("Volver a pedir al servidor sus pantallas conectadas"),
    ),
    (Msg::DisplaySlotHeading, t("Salida {slot}")),
    (Msg::DisplayStatusOff, t("SIN USO")),
    (Msg::DisplayStatusActive, t("ACTIVA")),
    (Msg::DisplayStatusUnknown, t("DESCONOCIDO")),
    (
        Msg::DisplayUnknownReconnecting,
        t("Desconocido (reconectando)"),
    ),
    (
        Msg::DisplayUnknownDisconnected,
        t("Desconocido (sin conexión con el servidor)"),
    ),
    (Msg::DisplayByName, t("{name}, por nombre")),
    (Msg::DisplayById, t("Pantalla {id}")),
    (
        Msg::DisplayByIndex,
        t("Pantalla {number} por posición en el escritorio"),
    ),
    (
        Msg::DisplayStatusHover,
        t("El estado en vivo de la salida {slot} según el servidor"),
    ),
    (Msg::DisplayNotConfigured, t("Sin configurar")),
    (Msg::DisplayActiveDetail, t("En {name} a {mode}; {hdr}")),
    (
        Msg::DisplayModeFallback,
        t("El modo pedido no está disponible, así que se usa el modo actual"),
    ),
    (Msg::DisplayUnsaved, t("Cambios sin guardar")),
    (Msg::DisplayPick, t("Pantalla de destino")),
    (
        Msg::DisplayPickHover,
        t(
            "Elija la pantalla que ocupa esta salida; se recuerda por el identificador estable de la pantalla",
        ),
    ),
    (Msg::DisplayNone, t("Elija una pantalla…")),
    (Msg::DisplayEntry, t("{name} · {mode}")),
    (
        Msg::DisplayOptionHover,
        t("{name} en {x}, {y}: ahora {mode}, {hdr}"),
    ),
    (Msg::DisplayNotConnected, t("{display} (no conectada)")),
    (
        Msg::DisplayNotConnectedHover,
        t("Conservar la pantalla guardada {display}; la salida espera hasta que se conecte"),
    ),
    (Msg::HdrCapHdr, t("admite HDR")),
    (Msg::HdrCapSdr, t("solo SDR")),
    (Msg::HdrCapUnknown, t("capacidad HDR desconocida")),
    (Msg::DisplaySource, t("Fuente")),
    (
        Msg::DisplaySourceHover,
        t(
            "Elija la imagen que muestra esta salida: Programa, Vista previa o la mezcla de una salida",
        ),
    ),
    (Msg::DisplaySourceProgram, t("Bus de programa")),
    (Msg::DisplaySourcePreview, t("Bus de vista previa")),
    (
        Msg::DisplaySourceOptionHover,
        t("Mostrar {source} en esta pantalla"),
    ),
    (Msg::DisplayModeLabel, t("Modo")),
    (
        Msg::DisplayModeHover,
        t(
            "Mantener el modo actual de la pantalla o cambiarlo a un modo exacto; un modo que la pantalla no ofrece vuelve al actual",
        ),
    ),
    (Msg::DisplayModeCurrent, t("Modo actual")),
    (
        Msg::DisplayModeCurrentHover,
        t("Dejar la pantalla en el modo que ya usa"),
    ),
    (
        Msg::DisplayModeOptionHover,
        t("Cambiar la pantalla a {mode} mientras se muestra esta salida"),
    ),
    (Msg::DisplayScaling, t("Escalado")),
    (
        Msg::DisplayScalingHover,
        t("Cómo se ajusta la imagen a la pantalla"),
    ),
    (Msg::ScalingFit, t("Ajustar dentro")),
    (Msg::ScalingFill, t("Rellenar")),
    (Msg::ScalingStretch, t("Estirar")),
    (Msg::ScalingOneToOne, t("Píxeles 1:1")),
    (
        Msg::ScalingFitHover,
        t("Mostrar toda la imagen con barras negras donde las formas difieren"),
    ),
    (
        Msg::ScalingFillHover,
        t("Cubrir toda la pantalla, recortando lo que sobra de la imagen"),
    ),
    (
        Msg::ScalingStretchHover,
        t("Estirar cada eje para cubrir la pantalla exactamente"),
    ),
    (
        Msg::ScalingOneToOneHover,
        t("Un píxel de imagen por píxel de pantalla, centrado; recorta o añade bordes"),
    ),
    (Msg::DisplayHdr, t("Alto rango (HDR)")),
    (
        Msg::DisplayHdrHover,
        t(
            "Desactivado mantiene SDR; Automático usa HDR cuando la pantalla y el presentador lo admiten ({capability})",
        ),
    ),
    (Msg::HdrOff, t("Desactivado (SDR)")),
    (Msg::HdrAuto, t("Automático")),
    (Msg::HdrOffHover, t("Presentar siempre esta salida en SDR")),
    (
        Msg::HdrAutoHover,
        t("Presentar en HDR cuando la pantalla y el presentador lo admiten; si no, en SDR"),
    ),
    (Msg::DisplayApply, t("Guardar y aplicar")),
    (
        Msg::DisplayApplyHover,
        t("Guardar la salida {slot} en el proyecto y aplicarla en vivo (se puede deshacer)"),
    ),
    (Msg::DisplayRevert, t("Descartar cambios")),
    (
        Msg::DisplayRevertHover,
        t("Descartar los cambios sin guardar de la salida {slot}"),
    ),
    (Msg::DisplayClear, t("Quitar")),
    (
        Msg::DisplayClearHover,
        t("Quitar la salida {slot} del proyecto y cerrar su ventana (se puede deshacer)"),
    ),
    (
        Msg::ReasonDisplayNoChanges,
        t("esta salida no tiene cambios sin guardar"),
    ),
    (
        Msg::ReasonDisplayNotConfigured,
        t("esta salida no está configurada"),
    ),
    (
        Msg::ReasonDisplayChooseDisplay,
        t("elija primero una pantalla"),
    ),
    (
        Msg::DisplayApplying,
        t("Aplicando: esperando la confirmación del servidor"),
    ),
    (
        Msg::DisplayRefused,
        t("El servidor rechazó el cambio: {reason}. Sus cambios se conservan."),
    ),
    (
        Msg::DisplayChangedElsewhere,
        t("La salida {slot} se cambió en otro lugar desde que empezó a editarla"),
    ),
    (Msg::DisplayReload, t("Recargar")),
    (
        Msg::DisplayReloadHover,
        t(
            "Descartar sus cambios en la salida {slot} y empezar de nuevo desde su configuración actual",
        ),
    ),
    (Msg::DisplayOverwrite, t("Sobrescribir igualmente")),
    (
        Msg::DisplayOverwriteHover,
        t(
            "Guardar de todos modos su versión de la salida {slot}, reemplazando el cambio hecho en otro lugar (se puede deshacer)",
        ),
    ),
    (
        Msg::ReasonDisplayChangedElsewhere,
        t("esta salida se cambió en otro lugar; recárguela o sobrescríbala"),
    ),
    (
        Msg::ReasonDisplayApplying,
        t("el servidor aún no ha respondido al último cambio"),
    ),
    (Msg::WaitDisplayAbsent, t("La pantalla no está conectada")),
    (Msg::WaitDisplayInUse, t("Otra salida usa esta pantalla")),
    (
        Msg::WaitAmbiguousName,
        t("Varias pantallas comparten este nombre"),
    ),
    (
        Msg::WaitEnumerationFailed,
        t("No se pudo leer la lista de pantallas"),
    ),
    (
        Msg::WaitNoPresenter,
        t("Este servidor no abre ventanas a pantalla completa"),
    ),
    (Msg::HdrStateSdr, t("rango estándar (SDR)")),
    (Msg::HdrStateHdr, t("HDR activo")),
    (Msg::HdrStateNotCapable, t("SDR, la pantalla no admite HDR")),
    (Msg::HdrStateUnknown, t("SDR, capacidad HDR desconocida")),
    (
        Msg::HdrStatePresenterLimited,
        t("SDR, el presentador aún no puede mostrar HDR"),
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
    (Msg::MasterTap, t("PRE-STINGER/FTB")),
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
            "Añade al final del banco una entrada generada en vivo, un flujo de red, una cámara, el audio del sistema, una imagen fija, una presentación, una lista de medios o una captura de ventana o de pantalla, con un canal de mezclador predeterminado. Las entradas de medios se añaden sin conexión.",
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
    (Msg::NameNetworkStream, t("Flujo de red")),
    (Msg::KindNetworkKey, t("FLUJO DE RED")),
    (
        Msg::KindNetworkHover,
        t(
            "Recibe un flujo RTSP, RTP, UDP, TCP o HTTP MPEG-TS, una lista HLS, un flujo RTMP (recibido de un servidor o enviado por un codificador) o un flujo SRT, con un búfer de fluctuación acotado y reconexión automática",
        ),
    ),
    (Msg::NsUrl, t("URL del flujo")),
    (
        Msg::NsUrlHover,
        t(
            "Dirección rtsp://, rtp://, udp://, tcp://, http://, https://, rtmp://, rtmps:// o srt:// del flujo (una dirección .m3u8 es HLS). Para escuchar, indica la dirección y el puerto locales, como rtmp://0.0.0.0:1935/live o srt://0.0.0.0:9000. Las credenciales y claves de la URL se guardan con el proyecto pero no se vuelven a mostrar",
        ),
    ),
    (Msg::NsTransport, t("Transporte RTSP")),
    (Msg::NsTransportAuto, t("AUTO")),
    (Msg::NsTransportTcp, t("TCP")),
    (Msg::NsTransportUdp, t("UDP")),
    (
        Msg::NsTransportHover,
        t(
            "Cómo transporta su contenido un flujo RTSP: Auto deja elegir al servidor, TCP lo intercala en la conexión de control (atraviesa cortafuegos), UDP tiene la menor latencia",
        ),
    ),
    (Msg::NsJitter, t("Búfer de fluctuación (ms)")),
    (
        Msg::NsJitterHover,
        t(
            "Latencia que la entrada retiene para absorber la fluctuación de red, de 40 a 5000 ms; el búfer nunca guarda más del doble",
        ),
    ),
    (Msg::NsBackoffMin, t("Primer reintento tras (ms)")),
    (
        Msg::NsBackoffMinHover,
        t(
            "Espera antes del primer intento de reconexión; cada intento siguiente la duplica hasta el máximo",
        ),
    ),
    (Msg::NsBackoffMax, t("Espera máxima entre reintentos (ms)")),
    (
        Msg::NsBackoffMaxHover,
        t("La espera más larga entre intentos de reconexión, de 100 a 60000 ms"),
    ),
    (Msg::NsGiveUp, t("Abandonar tras (intentos)")),
    (
        Msg::NsGiveUpHover,
        t(
            "Intentos de reconexión seguidos antes de que la entrada muestre FALLIDO; 0 reintenta mientras la entrada esté abierta",
        ),
    ),
    (Msg::NsGiveUpNever, t("0 = no abandonar nunca")),
    (Msg::NsTimeout, t("Tiempo de espera (ms)")),
    (
        Msg::NsTimeoutHover,
        t(
            "Cuánto tiempo sin datos antes de dar el flujo por perdido y reconectar, de 500 a 60000 ms",
        ),
    ),
    (Msg::NsStall, t("Durante un corte")),
    (Msg::NsStallFreeze, t("MANTENER ÚLTIMO CUADRO")),
    (
        Msg::NsStallFreezeHover,
        t(
            "Mientras el flujo se corta o reconecta, la entrada mantiene su última imagen y su audio se desvanece a silencio",
        ),
    ),
    (
        Msg::NsStallBlackHover,
        t(
            "Mientras el flujo se corta o reconecta, la entrada muestra negro y su audio se desvanece a silencio",
        ),
    ),
    (Msg::NsReasonUrlEmpty, t("escribe la URL del flujo")),
    (
        Msg::NsReasonUrlInvalid,
        t(
            "la URL no es una dirección de flujo (una URL rtmp:// nombra su aplicación, una srt:// su puerto y ninguna consulta)",
        ),
    ),
    (
        Msg::NsReasonTransport,
        t("el transporte solo se aplica a flujos rtsp://"),
    ),
    (
        Msg::NsReasonBackoff,
        t("la primera espera no puede superar la máxima"),
    ),
    (
        Msg::NsReasonSettings,
        t("un ajuste del flujo está fuera de rango"),
    ),
    (Msg::NsBadgeIdle, t("INACTIVO")),
    (Msg::NsBadgeBuffering, t("ALMACENANDO")),
    (Msg::NsBadgeStalled, t("CORTADO")),
    (Msg::NsBadgeListening, t("ESCUCHANDO")),
    (Msg::NsMode, t("Modo de conexión")),
    (Msg::NsModeCaller, t("LLAMADA")),
    (Msg::NsModeListener, t("ESCUCHA")),
    (Msg::NsModeRendezvous, t("ENCUENTRO")),
    (
        Msg::NsModeCallerHover,
        t("Se conecta a la dirección de la URL y recibe el flujo de ese servidor"),
    ),
    (
        Msg::NsModeListenerHover,
        t(
            "Espera en la dirección y el puerto locales de la URL a que un codificador (como OBS) envíe el flujo; cada entrada necesita su propio puerto, 1024 o superior",
        ),
    ),
    (
        Msg::NsModeRendezvousHover,
        t(
            "Solo SRT: ambos extremos se conectan entre sí en el mismo puerto a la vez, lo que atraviesa cortafuegos sin un extremo a la escucha",
        ),
    ),
    (Msg::NsStreamKey, t("Clave de transmisión")),
    (
        Msg::NsStreamKeyHover,
        t(
            "La clave RTMP tras la aplicación (rtmp://host/app/CLAVE): al recibir de un servidor nombra el flujo; al escuchar, quien publique debe usarla o se le desconecta. Se guarda con el proyecto y no se vuelve a mostrar",
        ),
    ),
    (Msg::NsPassphrase, t("Frase de cifrado")),
    (
        Msg::NsPassphraseHover,
        t(
            "Frase de cifrado SRT, de 10 a 79 caracteres, igual en ambos extremos; déjala vacía para un flujo sin cifrar. Se guarda con el proyecto y no se vuelve a mostrar",
        ),
    ),
    (Msg::NsKeyLength, t("Cifrado")),
    (
        Msg::NsKeyLengthHover,
        t("La longitud de clave AES que deriva la frase SRT; ambos extremos deben coincidir"),
    ),
    (Msg::NsStreamId, t("ID de flujo")),
    (
        Msg::NsStreamIdHover,
        t(
            "El ID de flujo SRT que se envía al otro extremo, que algunos servidores usan para elegir o autorizar un flujo; hasta 512 caracteres, puede quedar vacío",
        ),
    ),
    (Msg::NsLatency, t("Latencia SRT (ms)")),
    (
        Msg::NsLatencyHover,
        t(
            "Cuánto espera SRT para recuperar paquetes perdidos antes de descartarlos, de 20 a 8000 ms; unas cuatro veces el tiempo de ida y vuelta es un buen comienzo",
        ),
    ),
    (Msg::NsAudio, t("Canales de audio")),
    (Msg::NsAudioMix, t("MEZCLA")),
    (
        Msg::NsAudioMixHover,
        t("Escucha todos los canales de la fuente mezclados en estéreo"),
    ),
    (
        Msg::NsAudioPairHover,
        t(
            "Escucha solo los canales {left} y {right} de la fuente, como par estéreo; para una fuente con pares estéreo separados, añade una entrada por par",
        ),
    ),
    (
        Msg::NsReasonMode,
        t(
            "solo los flujos rtmp:// y srt:// pueden escuchar (rtmps:// no), y solo srt:// admite encuentro",
        ),
    ),
    (
        Msg::NsReasonListenPort,
        t("una entrada a la escucha necesita su propio puerto, 1024 o superior"),
    ),
    (
        Msg::NsReasonStreamKey,
        t(
            "una clave de transmisión es texto imprimible sin espacios, y entonces la URL solo nombra la aplicación",
        ),
    ),
    (
        Msg::NsReasonPassphrase,
        t("una frase de cifrado SRT tiene de 10 a 79 caracteres"),
    ),
    (
        Msg::NsReasonStreamId,
        t("un ID de flujo SRT es texto imprimible de 512 caracteres como máximo"),
    ),
    (
        Msg::NsReasonKeyLength,
        t("escribe una frase de cifrado para cifrar el flujo"),
    ),
    (
        Msg::NsDetailRtmp,
        t("RTMP {mode}, clave de transmisión {key}"),
    ),
    (Msg::NsDetailKeySet, t("establecida")),
    (Msg::NsDetailKeyNone, t("sin establecer")),
    (
        Msg::NsDetailSrt,
        t("SRT {mode}, {latency} ms de latencia, {encryption}"),
    ),
    (Msg::NsDetailUnencrypted, t("sin cifrar")),
    (
        Msg::NsDetailAudio,
        t("Audio de la fuente: {count} canales, se escucha {pair}"),
    ),
    (Msg::NsBadgeReconnecting, t("RECONECTANDO {attempt}")),
    (Msg::NsDetailStream, t("Flujo de red {url}: {state}")),
    (Msg::NsDetailRates, t("{kbps} kb/s, {buffer} ms en búfer")),
    (
        Msg::NsDetailCounters,
        t("{dropped} cuadros descartados, {late} tardíos, {reconnects} reconexiones"),
    ),
    (
        Msg::NsDetailNextAttempt,
        t("Próximo intento en {seconds} s"),
    ),
    (Msg::NsSectionSource, t("Origen del flujo")),
    (Msg::NsSectionConnection, t("Conexión")),
    (Msg::NsSectionRecovery, t("Recuperación")),
    (Msg::NsUrlRecognised, t("Flujo {protocol}")),
    (
        Msg::NsConnectionDefaults,
        t(
            "Este protocolo se conecta con sus valores por defecto; Avanzado tiene el búfer de fluctuación",
        ),
    ),
    (
        Msg::NsConnectionPending,
        t("Los ajustes de conexión dependen del protocolo de la URL"),
    ),
    (Msg::AdvancedKey, t("AVANZADO")),
    (
        Msg::NsAdvSourceShow,
        t("Muestra los canales de audio que oye la entrada"),
    ),
    (
        Msg::NsAdvSourceHide,
        t("Oculta los canales de audio (la elección se conserva)"),
    ),
    (
        Msg::NsAdvConnectionShow,
        t(
            "Muestra el búfer de fluctuación y, para SRT, el ID de flujo y la longitud de la clave de cifrado",
        ),
    ),
    (
        Msg::NsAdvConnectionHide,
        t("Oculta los ajustes de conexión menos habituales (sus valores se conservan)"),
    ),
    (
        Msg::NsAdvRecoveryShow,
        t("Muestra las esperas de reconexión y el tiempo de espera sin datos"),
    ),
    (
        Msg::NsAdvRecoveryHide,
        t("Oculta las esperas de reconexión y el tiempo de espera (sus valores se conservan)"),
    ),
    (Msg::SecretShow, t("Mostrar secreto")),
    (Msg::SecretHide, t("Ocultar secreto")),
    (
        Msg::SecretShowHover,
        t("Muestra lo escrito aquí; los secretos se ocultan por defecto"),
    ),
    (Msg::SecretHideHover, t("Vuelve a ocultar lo escrito aquí")),
    (
        Msg::AddInputFixField,
        t("Corrige el campo marcado en rojo: {field}"),
    ),
    (Msg::NameCapture, t("Cámara")),
    (Msg::KindCaptureKey, t("CÁMARA / CAPTURA")),
    (
        Msg::KindCaptureHover,
        t(
            "Una cámara o tarjeta de captura conectada al servidor, con su propio formato, audio e imagen de pérdida de señal; se vuelve a adquirir automáticamente al desconectarla y reconectarla",
        ),
    ),
    (Msg::CapDevice, t("Dispositivo de captura")),
    (Msg::CapRefresh, t("Volver a buscar dispositivos")),
    (
        Msg::CapRefreshHover,
        t(
            "Pide al servidor que vuelva a listar sus dispositivos de captura, por ejemplo tras conectar una cámara",
        ),
    ),
    (
        Msg::CapInventoryUnknown,
        t("Pidiendo al servidor sus dispositivos de captura…"),
    ),
    (
        Msg::CapInventoryNoBackend,
        t(
            "Este servidor no tiene sistema de captura: no puede abrir cámaras ni tarjetas de captura",
        ),
    ),
    (
        Msg::CapInventoryEmpty,
        t(
            "No se encontraron dispositivos de captura ({backend}); conecta una cámara y vuelve a buscar",
        ),
    ),
    (
        Msg::CapInventoryCount,
        plural(
            "{count} dispositivo de captura encontrado ({backend})",
            "{count} dispositivos de captura encontrados ({backend})",
        ),
    ),
    (
        Msg::CapInventoryError,
        t("Falló la búsqueda de dispositivos: {error}"),
    ),
    (Msg::CapDeviceOption, t("{name} · {path} · {bus}")),
    (
        Msg::CapDeviceHover,
        t(
            "{name} en {path} (bus {bus}, controlador {driver}); {formats} formatos; audio emparejado: {audio}",
        ),
    ),
    (Msg::CapPairedNone, t("ninguno emparejado")),
    (Msg::CapFormat, t("Formato")),
    (
        Msg::CapFormatAuto,
        t("Automático (el más cercano al programa)"),
    ),
    (
        Msg::CapFormatAutoHover,
        t(
            "Deja que el servidor abra el formato del dispositivo más cercano al tamaño de imagen y la cadencia del programa, con preferencia por el barrido progresivo",
        ),
    ),
    (
        Msg::CapFormatOptionHover,
        t("Abre el dispositivo en {format}: {class}, {scan}"),
    ),
    (Msg::CapScanProgressive, t("progresivo")),
    (Msg::CapScanTff, t("entrelazado, campo superior primero")),
    (Msg::CapScanBff, t("entrelazado, campo inferior primero")),
    (Msg::CapFilterSize, t("Tamaño de imagen")),
    (Msg::CapFilterRate, t("Cadencia")),
    (Msg::CapFilterPixel, t("Formato de píxel")),
    (Msg::CapFilterAll, t("Cualquiera")),
    (
        Msg::CapFilterSizeHover,
        t("Muestra solo los formatos del dispositivo con este tamaño de imagen"),
    ),
    (
        Msg::CapFilterRateHover,
        t(
            "Muestra solo los formatos del dispositivo con esta cadencia de cuadros (un formato entrelazado lleva el doble de campos)",
        ),
    ),
    (
        Msg::CapFilterPixelHover,
        t(
            "Muestra solo los formatos del dispositivo con este formato de píxel: YUYV y NV12 no están comprimidos, MJPEG está comprimido y hay que decodificarlo",
        ),
    ),
    (
        Msg::CapFilterNone,
        t("Ningún formato coincide con estos filtros"),
    ),
    (
        Msg::CapAdvFormatShow,
        t(
            "Muestra los filtros de tamaño de imagen, cadencia y formato de píxel de la lista de formatos",
        ),
    ),
    (
        Msg::CapAdvFormatHide,
        t("Oculta los filtros de formato y vuelve a listar todos los formatos"),
    ),
    (Msg::CapRateFps, t("{rate} cps")),
    (Msg::CapAudioNone, t("NINGUNO")),
    (Msg::CapAudioPaired, t("AUDIO DEL DISPOSITIVO")),
    (Msg::CapAudioAlsa, t("DISPOSITIVO ALSA")),
    (Msg::CapAudioNoneHover, t("La entrada queda en silencio")),
    (
        Msg::CapAudioPairedHover,
        t(
            "La entrada oye el dispositivo de audio de la misma cámara o tarjeta, si lo tiene; si no, queda en silencio",
        ),
    ),
    (
        Msg::CapAudioAlsaHover,
        t(
            "La entrada oye un dispositivo de captura ALSA con nombre en el servidor, como hw:CARD=C920,DEV=0",
        ),
    ),
    (Msg::CapPairedAudio, t("Audio emparejado: {audio}")),
    (Msg::CapAlsaDevice, t("Nombre del dispositivo ALSA")),
    (
        Msg::CapAlsaDeviceHover,
        t(
            "El nombre del dispositivo de captura ALSA en el servidor: de 1 a 64 caracteres imprimibles sin espacios, como los lista arecord -L",
        ),
    ),
    (Msg::CapLoss, t("Al perder la señal")),
    (Msg::CapLossHold, t("MANTENER ÚLTIMA IMAGEN")),
    (
        Msg::CapLossHoldHover,
        t("Mientras se pierde la señal, la entrada mantiene su última imagen y queda en silencio"),
    ),
    (
        Msg::CapLossBlackHover,
        t("Mientras se pierde la señal, la entrada muestra negro y queda en silencio"),
    ),
    (Msg::CapReasonNoDevice, t("elige un dispositivo de captura")),
    (
        Msg::CapReasonAlsa,
        t(
            "el nombre del dispositivo ALSA debe tener de 1 a 64 caracteres imprimibles sin espacios",
        ),
    ),
    (
        Msg::CapReasonSettings,
        t("un ajuste de captura está fuera de rango"),
    ),
    (Msg::CapBadgeSignalLost, t("SIN SEÑAL")),
    (Msg::CapLostUnplugged, t("dispositivo desconectado")),
    (Msg::CapLostStalled, t("no llegan imágenes")),
    (Msg::CapLostNoSignal, t("sin señal de entrada")),
    (Msg::CapLostError, t("error de adquisición")),
    (Msg::CapDetailDevice, t("Captura {device}: {state}")),
    (Msg::CapDetailLost, t("{state} ({reason})")),
    (Msg::CapDetailFormat, t("{format}, {audio}")),
    (Msg::CapDetailNoFormat, t("formato sin negociar")),
    (Msg::CapDetailAudioOn, t("audio activado")),
    (Msg::CapDetailAudioOff, t("audio desactivado")),
    (
        Msg::CapDetailCounters,
        t("{losses} pérdidas, {recoveries} recuperaciones, {renegotiations} renegociaciones"),
    ),
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
    (Msg::StreamDiagFailure, t("Último error: {failure}")),
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
    // Output failures (Protocol 2.67) and recorder availability.
    (
        Msg::FailConnectFailed,
        t("No se pudo conectar con el servidor"),
    ),
    (
        Msg::FailConnectionRefused,
        t("El servidor rechazó la conexión"),
    ),
    (
        Msg::FailConnectTimeout,
        t("El servidor no respondió a tiempo"),
    ),
    (Msg::FailNetworkUnreachable, t("Red inaccesible")),
    (
        Msg::FailHostNotFound,
        t("No se encontró el nombre del servidor"),
    ),
    (Msg::FailTlsFailed, t("Falló la conexión segura")),
    (
        Msg::FailStreamKeyRejected,
        t("El servidor rechazó la clave de transmisión"),
    ),
    (
        Msg::FailConnectionLost,
        t("Se perdió la conexión con el servidor"),
    ),
    (Msg::FailStreamStalled, t("La transmisión se atascó")),
    (
        Msg::FailProtocolError,
        t("El destino no acepta esta transmisión"),
    ),
    (
        Msg::FailReconnectExhausted,
        t("Se abandonó tras varios intentos de reconexión"),
    ),
    (
        Msg::FailFfmpegUnavailable,
        t("FFmpeg falta o no se puede usar"),
    ),
    (Msg::FailFfmpegExited, t("FFmpeg se detuvo inesperadamente")),
    (Msg::FailDiskFull, t("Disco lleno")),
    (
        Msg::FailRecordingFolderNotWritable,
        t("No se puede escribir en la carpeta de grabación"),
    ),
    (
        Msg::FailOutputFolderNotWritable,
        t("No se puede escribir en la carpeta de salida"),
    ),
    (
        Msg::FailRecordingFolderMissing,
        t("No se encuentra la carpeta de grabación"),
    ),
    (
        Msg::FailOutputFolderMissing,
        t("No se encuentra la carpeta de salida"),
    ),
    (Msg::FailWriteError, t("Error de escritura en disco")),
    (Msg::FailStartupTimeout, t("No arrancó a tiempo")),
    (
        Msg::FailFellBehind,
        t("No pudo seguir el ritmo; se perdieron fotogramas"),
    ),
    (
        Msg::FailCaptureFailed,
        t("Falló la captura de la imagen de Programa"),
    ),
    (
        Msg::FailInternalError,
        t("Error interno; vea el registro del daemon"),
    ),
    (
        Msg::FailUnknown,
        t("Error desconocido; vea el registro del daemon"),
    ),
    (
        Msg::FailWithExitCode,
        t("{reason} (FFmpeg terminó con código {code})"),
    ),
    (Msg::RecorderNotConfigured, t("SIN CONFIGURAR")),
    (
        Msg::RowRecorderNotConfigured,
        t("Esta ranura no tiene carpeta de grabación"),
    ),
    (
        Msg::ReasonRecorderNotConfigured,
        t("este grabador no tiene carpeta de grabación (inicie freemixd con --record-program)"),
    ),
    // Input fallback name and catalogue refusals.
    (Msg::InputFallbackName, t("Entrada {input}")),
    (Msg::AddRefused, t("Alta rechazada: {advice}")),
    (Msg::RemoveRefused, t("Baja rechazada: {advice}")),
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
    (
        Msg::InUseSceneLayer,
        t("«{name}» es una fuente de capa de la escena {scene}."),
    ),
    (
        Msg::InUseSceneAudio,
        t("«{name}» es la fuente de audio de la escena «{scene}»."),
    ),
    (
        Msg::InUseAliasTarget,
        t("«{name}» es el destino del alias «{alias}»."),
    ),
    (
        Msg::InUsePlaylistEntry,
        t("«{name}» se reproduce en la entrada {entry} de la lista."),
    ),
    (
        Msg::InUseSceneOnAir,
        t("«{name}» se muestra en una escena al aire."),
    ),
    (
        Msg::AdviceSceneLayer,
        t("Quite esa capa de la escena primero."),
    ),
    (Msg::AdvicePlaylist, t("Quítela de la lista primero.")),
    (
        Msg::AdviceSceneOffAir,
        t("Saque esa escena del aire primero."),
    ),
    // Typed command refusals (Protocol 2.45).
    (Msg::CommandRefused, t("Comando rechazado: {reason}")),
    (Msg::ToastReasonCode, t("Código del motivo: {code}")),
    (
        Msg::RsnPermissionDenied,
        t("Esta sesión no tiene permiso para hacerlo."),
    ),
    (
        Msg::RsnDeadlineExceeded,
        t("El comando llegó después de su plazo; inténtelo de nuevo."),
    ),
    (
        Msg::RsnRevisionConflict,
        t("El proyecto pasó a la revisión {revision}; inténtelo de nuevo."),
    ),
    (Msg::RsnUnknownId, t("{what} ya no existe.")),
    (Msg::RsnDuplicateId, t("{what} ya existe.")),
    (
        Msg::RsnDuplicateName,
        t("Ese nombre ya está en uso; elija otro."),
    ),
    (
        Msg::RsnInvalidName,
        t("Use un nombre no vacío de {max} bytes como máximo."),
    ),
    (
        Msg::RsnProjectLocked,
        t("El proyecto está bloqueado; abra el candado de la barra superior para editar."),
    ),
    (
        Msg::RsnInputLocked,
        t("«{name}» tiene su propio candado; ábralo primero."),
    ),
    (
        Msg::RsnDisplayInUse,
        t("Esa pantalla ya la usa la salida de pantalla {slot}."),
    ),
    (Msg::RsnBoundsExceeded, t("{field}: el límite es {max}.")),
    (Msg::RsnNothingToUndo, t("No queda nada que deshacer.")),
    (Msg::RsnNothingToRedo, t("No queda nada que rehacer.")),
    (
        Msg::RsnHistoryConflict,
        t("Otra persona editó primero; su edición es la siguiente del historial."),
    ),
    (
        Msg::RsnHistoryDrift,
        t("El ajuste editado cambió desde entonces; deshágalo o rehágalo a mano."),
    ),
    (
        Msg::RsnStingerRealizing,
        t("El Stinger {slot} aún se está preparando; inténtelo de nuevo en un momento."),
    ),
    (
        Msg::RsnPlaylistTakeScheduled,
        t("Hay una toma de la lista programada para el cuadro {frame}; espérela."),
    ),
    (
        Msg::RsnNotDurable,
        t("El daemon no pudo guardar este cambio, así que no cambió nada; inténtelo de nuevo."),
    ),
    (
        Msg::RsnRateLimited,
        t("Demasiados comandos a la vez; inténtelo de nuevo en un momento."),
    ),
    (
        Msg::RsnRuntimeUnsupported,
        t(
            "El motor de medios de este daemon no puede hacerlo en marcha; haga el cambio sin conexión.",
        ),
    ),
    (Msg::IdKindInput, t("Entrada {id}")),
    (Msg::IdKindOutput, t("Salida {id}")),
    (Msg::IdKindCategory, t("Categoría {id}")),
    (Msg::IdKindGrade, t("Corrección de color {id}")),
    (Msg::IdKindLut, t("LUT {id}")),
    (Msg::IdKindPreset, t("Preset de animación {id}")),
    (Msg::IdKindTitle, t("Título {id}")),
    (Msg::IdKindPlaylistEntry, t("Entrada de lista {id}")),
    (Msg::IdKindTitleNamed, t("Título «{name}»")),
    (
        Msg::RsnUnsupportedScheme,
        t(
            "esquema no admitido (admitidos: rtsp, rtp, udp, tcp, http, https, hls, rtmp, rtmps, srt)",
        ),
    ),
    (Msg::BoundsFieldInputs, t("Entradas del proyecto")),
    (Msg::BoundsFieldPlaylistEntries, t("Entradas de lista")),
    (Msg::BoundsFieldGrades, t("Correcciones de color")),
    (Msg::BoundsFieldLuts, t("LUT")),
    (Msg::BoundsFieldLayerGrades, t("Correcciones de capa")),
    (Msg::BoundsFieldSceneLayers, t("Capas de escena")),
    (Msg::BoundsFieldLayerEffects, t("Efectos de capa")),
    (Msg::BoundsFieldPresets, t("Presets de animación")),
    (
        Msg::BoundsFieldTitleContentBytes,
        t("Bytes de contenido del título"),
    ),
    (
        Msg::BoundsFieldTitleTextChars,
        t("Caracteres de texto del título"),
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
    (Msg::TitleFieldWeb, t("En la web")),
    (
        Msg::TitleFieldWebHover,
        t(
            "Mostrar este campo en el editor de títulos del navegador, en todos los títulos de esta plantilla. Un campo sin marcar nunca se envía a las páginas web.",
        ),
    ),
    (
        Msg::TitleTimelineHover,
        t(
            "Las fases de entrada, en aire y salida del título; el cursor sigue al servidor. Solo lectura: edite las fases en la ventana Animación.",
        ),
    ),
    (Msg::TitleTimelineAir, t("Al aire {seconds} s")),
    (Msg::TitleTimelinePreview, t("En Previo {seconds} s")),
    (Msg::TitleTimelineIdle, t("Sin mostrar")),
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
    (Msg::AnimKeyButton, t("♦ Clave")),
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
        t("Re Pág, o flecha izquierda sin claves seleccionadas"),
    ),
    (
        Msg::AnimKeyStepForward,
        t("Av Pág, o flecha derecha sin claves seleccionadas"),
    ),
    (Msg::AnimKeyDelete, t("Supr")),
    (Msg::AnimKeyRedo, t("Ctrl+Mayús+Z")),
    (Msg::AnimKeyPreviousKey, t("Ctrl+flecha izquierda")),
    (Msg::AnimKeyNextKey, t("Ctrl+flecha derecha")),
    (
        Msg::AnimPreviousKeyHover,
        t(
            "Llevar el cabezal a la clave anterior de esta fase. Seleccione claves, o haga clic en una fila en el modo Avanzado, para limitarse a esas filas.",
        ),
    ),
    (
        Msg::AnimPreviousKeyRowsHover,
        t(
            "Llevar el cabezal a la clave anterior de las filas con claves seleccionadas (sin nada seleccionado, la fila activa)",
        ),
    ),
    (
        Msg::AnimNextKeyHover,
        t(
            "Llevar el cabezal a la clave siguiente de esta fase. Seleccione claves, o haga clic en una fila en el modo Avanzado, para limitarse a esas filas.",
        ),
    ),
    (
        Msg::AnimNextKeyRowsHover,
        t(
            "Llevar el cabezal a la clave siguiente de las filas con claves seleccionadas (sin nada seleccionado, la fila activa)",
        ),
    ),
    (
        Msg::AnimReasonNoPreviousKey,
        t("no hay ninguna clave antes del cabezal en esta fase"),
    ),
    (
        Msg::AnimReasonNoNextKey,
        t("no hay ninguna clave después del cabezal en esta fase"),
    ),
    (
        Msg::AnimReasonNoPreviousKeyRows,
        t("no hay ninguna clave antes del cabezal en las filas seleccionadas o activa"),
    ),
    (
        Msg::AnimReasonNoNextKeyRows,
        t("no hay ninguna clave después del cabezal en las filas seleccionadas o activa"),
    ),
    (Msg::AnimSnap, t("Imán")),
    (
        Msg::AnimSnapOnHover,
        t(
            "El imán está activado: las claves, los tiradores y el cabezal arrastrados se pegan a otras claves, al cabezal, a los extremos de la fase y a los segundos enteros.\nMantenga Mayús al arrastrar para moverlos libremente. Haga clic para desactivar el imán.",
        ),
    ),
    (
        Msg::AnimSnapOffHover,
        t(
            "El imán está desactivado: los arrastres avanzan fotograma a fotograma sin pegarse.\nMantenga Mayús al arrastrar para pegarlos igualmente. Haga clic para activar el imán.",
        ),
    ),
    (Msg::AnimStagger, t("Escalonar…")),
    (
        Msg::AnimStaggerHover,
        t(
            "Escalonar los elementos para que la animación de cada uno empiece unos fotogramas después de la anterior (barra, luego nombre, luego cargo). Actúa sobre los elementos con claves seleccionadas si hay dos o más; si no, sobre todos los elementos animados. Un solo paso de deshacer.",
        ),
    ),
    (Msg::AnimStaggerStep, t("Separación")),
    (
        Msg::AnimStaggerStepHover,
        t(
            "Fotogramas entre el inicio de un elemento y el del siguiente. Con 0, todos los elementos empiezan a la vez.",
        ),
    ),
    (Msg::AnimStaggerTopFirst, t("Arriba primero")),
    (
        Msg::AnimStaggerTopFirstHover,
        t("La fila de arriba empieza primero y luego cada fila de debajo"),
    ),
    (Msg::AnimStaggerBottomFirst, t("Abajo primero")),
    (
        Msg::AnimStaggerBottomFirstHover,
        t(
            "La fila de abajo empieza primero y luego cada fila de encima (útil para una SALIDA que deshace la ENTRADA)",
        ),
    ),
    (
        Msg::AnimStaggerOrderHover,
        t("Los elementos en el orden en que empezarán"),
    ),
    (Msg::AnimStaggerApply, t("Aplicar escalonado")),
    (
        Msg::AnimReasonTooFewTargets,
        t("anime primero al menos dos elementos en esta fase"),
    ),
    (
        Msg::AnimErrTooFewTargets,
        t("Para escalonar hacen falta al menos dos elementos animados en esta fase"),
    ),
    (
        Msg::AnimStaggerApplyHover,
        plural(
            "Hacer que {count} elemento empiece {step} después del anterior; cada uno conserva su ritmo y su suavizado, y la fase se alarga si el último terminara después de ella. Un solo paso de deshacer. Mientras apunta aquí, la línea de tiempo muestra adónde irán las claves.",
            "Hacer que {count} elementos empiecen {step} después del anterior; cada uno conserva su ritmo y su suavizado, y la fase se alarga si el último terminara después de ella. Un solo paso de deshacer. Mientras apunta aquí, la línea de tiempo muestra adónde irán las claves.",
        ),
    ),
    (Msg::AnimStaggerSpread, t("Repartir hasta llenar la fase")),
    (
        Msg::AnimStaggerSpreadHover,
        t(
            "En lugar de una separación fija, repartir los inicios por igual para que el último elemento termine justo al final de la fase. Desactívelo para elegir usted la separación.",
        ),
    ),
    (
        Msg::AnimReasonSpreadSpacing,
        t("Repartir hasta llenar la fase decide la separación; desactívelo para elegirla"),
    ),
    (
        Msg::AnimStaggerSpreadApplyHover,
        plural(
            "Repartir {count} elemento por igual para que el último termine al final de la fase ({length}); cada uno conserva su ritmo y su suavizado, y la fase solo se alarga si alguno terminara después de ella. Un solo paso de deshacer. Mientras apunta aquí, la línea de tiempo muestra adónde irán las claves.",
            "Repartir {count} elementos por igual para que el último termine al final de la fase ({length}); cada uno conserva su ritmo y su suavizado, y la fase solo se alarga si alguno terminara después de ella. Un solo paso de deshacer. Mientras apunta aquí, la línea de tiempo muestra adónde irán las claves.",
        ),
    ),
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
        Msg::AnimRulerHoverSnapOff,
        t(
            "Regla de tiempo: haga clic o arrastre para mover el cabezal (el imán está desactivado; mantenga Mayús para ajustarlo a las claves). Ctrl+rueda amplía, Mayús+rueda desplaza.",
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
        Msg::AnimStripHoverSnapOff,
        t(
            "Tirador de tiempo: arrastre para estirar o comprimir en proporción las claves seleccionadas. Mantenga Alt para mover solo las claves de este borde; el imán está desactivado, así que Mayús ajusta.",
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
    (
        Msg::AnimKeyDragHelpSnapOff,
        t(
            "Arrastre para mover las claves seleccionadas (el imán está desactivado; mantenga Mayús para ajustarlas a claves, al cabezal y a segundos enteros). Ctrl+clic la añade o la quita de la selección; Mayús+clic selecciona un intervalo.",
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
        Msg::AnimBarEndHoverSnapOff,
        t(
            "Tirador de duración: arrastre para estirar o comprimir la animación de {name}. Mantenga Alt para mover solo este extremo; el imán está desactivado, así que Mayús ajusta.",
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
    (
        Msg::AnimRetimeModifiersSnapOff,
        t("Alt: mover un borde · Mayús: ajustar · Esc: cancelar"),
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
    (Msg::ColorStatusApplied, t("APLICADA")),
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
    (Msg::ColorCopyTo, t("Copiar a…")),
    (
        Msg::ColorCopyToHover,
        t(
            "Dar esta corrección a otras entradas, capas de escena o al Programa con un clic; cada destino tiene su propio paso de deshacer",
        ),
    ),
    (Msg::ColorCopyHeader, t("Copiar esta corrección a")),
    (
        Msg::ColorCopyHeaderHover,
        t(
            "Marque los destinos que deben recibir este borrador. Una corrección vinculada de la biblioteca sigue vinculada en cada uno",
        ),
    ),
    (Msg::ColorCopyEveryInput, t("Todas las entradas")),
    (
        Msg::ColorCopyEveryInputHover,
        t("Marcar todas las entradas que puede corregir, salvo la que está editando"),
    ),
    (Msg::ColorCopyUntick, t("Desmarcar todo")),
    (
        Msg::ColorCopyUntickHover,
        t("Desmarcar todos los destinos de esta lista"),
    ),
    (
        Msg::ColorCopyPickHover,
        t("Marcar para copiar aquí la corrección; reemplaza la corrección actual del destino"),
    ),
    (
        Msg::ColorCopyApply,
        plural("Copiar a {count} destino", "Copiar a {count} destinos"),
    ),
    (
        Msg::ColorCopyApplyHover,
        t(
            "Enviar ahora esta corrección a cada destino marcado; los monitores la muestran a medida que se aplica",
        ),
    ),
    (
        Msg::ColorCopySent,
        plural(
            "Corrección copiada a {count} destino (un paso de deshacer cada uno)",
            "Corrección copiada a {count} destinos (un paso de deshacer cada uno)",
        ),
    ),
    (
        Msg::ColorReasonCopyNone,
        t("marque al menos un destino primero"),
    ),
    (
        Msg::ColorReasonCopySame,
        t("todos los destinos marcados ya tienen esta corrección"),
    ),
    (Msg::ColorReasonCopyNothingTicked, t("no hay nada marcado")),
    (
        Msg::ColorReasonCopyNoOther,
        t("todas las entradas ya están marcadas o bloqueadas"),
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
        Msg::ColorSourceServer,
        t(
            "Imagen: la imagen de esta fuente enviada por el servidor, actualizada cada 3 s mientras no esté sin corregir en ningún monitor",
        ),
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
    // Layer keys and effects in the layer designer (IN-020).
    (Msg::DesignGroupKey, t("Clave")),
    (
        Msg::DesignKeyHover,
        t(
            "Hacer transparente parte de la capa: una clave de croma quita un color (una pantalla verde o azul) y una clave de luma quita zonas oscuras o claras",
        ),
    ),
    (
        Msg::DesignKeyKindHover,
        t("Elegir cómo se recorta la capa: sin clave, con clave de croma o con clave de luma"),
    ),
    (Msg::DesignKeyNone, t("Ninguna")),
    (
        Msg::DesignKeyNoneHover,
        t("Sin clave: se dibuja la capa entera"),
    ),
    (Msg::DesignKeyChroma, t("Croma")),
    (
        Msg::DesignKeyChromaHover,
        t("Quitar un color, como una pantalla verde o azul (empieza con verde puro)"),
    ),
    (Msg::DesignKeyLuma, t("Luma")),
    (
        Msg::DesignKeyLumaHover,
        t("Quitar zonas según su brillo (empieza quitando el negro)"),
    ),
    (
        Msg::DesignKeyColourHover,
        t("El color que quita la clave. Haga clic para elegirlo en una rueda de color"),
    ),
    (Msg::DesignAutoKey, t("Clave automática")),
    (
        Msg::DesignAutoKeyHover,
        t(
            "Recortar la capa con un clic: buscar el fondo verde o azul principal en la imagen de su entrada y ajustar con él el color de clave, la similitud, la suavidad, el derrame y el desderrame. Un solo cambio que Deshacer revierte",
        ),
    ),
    (Msg::DesignEyedropper, t("Elegir color")),
    (
        Msg::DesignEyedropperHover,
        t(
            "Después haga clic en el fondo en la imagen de la entrada de abajo: la clave toma ese color y ajusta el resto según el fondo parecido",
        ),
    ),
    (
        Msg::DesignKeyPictureHover,
        t(
            "La imagen actual de la entrada de la capa. Active Elegir color y haga clic en un punto del fondo para recortarlo",
        ),
    ),
    (
        Msg::DesignKeyPicturePickHover,
        t("Haga clic en un punto del fondo para recortar su color"),
    ),
    (
        Msg::DesignReasonNoInputPicture,
        t("la imagen de la entrada aún no ha llegado"),
    ),
    (
        Msg::DesignReasonSceneSource,
        t("la capa muestra una escena, que no tiene una única imagen de entrada que muestrear"),
    ),
    (
        Msg::DesignReasonAutoKeyOneLayer,
        t("seleccione una sola capa: cada capa se recorta según su propia entrada"),
    ),
    (
        Msg::DesignAutoKeyApplied,
        t(
            "Clave automática aplicada: color {colour}, similitud {similarity}, suavidad {smoothness}",
        ),
    ),
    (
        Msg::DesignAutoKeyNoBacking,
        t("La clave automática no encontró un fondo verde ni azul: elija el color del fondo"),
    ),
    (
        Msg::DesignAutoKeyGrey,
        t("Ese punto es demasiado gris para recortarlo: elija un punto del fondo con color"),
    ),
    (
        Msg::DesignAutoKeyUnreadable,
        t("La clave automática no pudo leer la imagen de la entrada"),
    ),
    (
        Msg::DesignKeyGreenHover,
        t("Quitar el verde puro, para una pantalla verde"),
    ),
    (
        Msg::DesignKeyBlueHover,
        t("Quitar el azul puro, para una pantalla azul"),
    ),
    (
        Msg::DesignKeyRedHover,
        t("Componente roja del color de la clave, de 0 a 255"),
    ),
    (
        Msg::DesignKeyGreenChannelHover,
        t("Componente verde del color de la clave, de 0 a 255"),
    ),
    (
        Msg::DesignKeyBlueChannelHover,
        t("Componente azul del color de la clave, de 0 a 255"),
    ),
    (Msg::DesignKeySimilarity, t("Similitud")),
    (
        Msg::DesignKeySimilarityHover,
        t(
            "Cuánto puede alejarse un color del de la clave y aun así quitarse: más alto quita más tonos",
        ),
    ),
    (Msg::DesignKeySmoothness, t("Suavidad")),
    (
        Msg::DesignKeySmoothnessHover,
        t(
            "Con qué gradualidad pasan los bordes de los colores quitados a los conservados: más alto da bordes más suaves",
        ),
    ),
    (Msg::DesignKeySpill, t("Derrame")),
    (
        Msg::DesignKeySpillHover,
        t(
            "Cuánto del tinte del color de la clave se quita de los bordes conservados, como los reflejos verdes en el pelo",
        ),
    ),
    (Msg::DesignLumaLow, t("Bajo")),
    (
        Msg::DesignLumaLowHover,
        t(
            "Brillo igual o inferior al que se quita la capa, de 0 a 254 (siempre por debajo de Alto)",
        ),
    ),
    (Msg::DesignLumaHigh, t("Alto")),
    (
        Msg::DesignLumaHighHover,
        t(
            "Brillo igual o superior al que también se quita la capa, de 1 a 255 (siempre por encima de Bajo); 255 no quita nada claro",
        ),
    ),
    (Msg::DesignLumaSoftness, t("Suavidad")),
    (
        Msg::DesignLumaSoftnessHover,
        t("Con qué gradualidad pasa el brillo quitado al conservado"),
    ),
    (
        Msg::DesignLumaInvertHover,
        t("Conservar solo el brillo que la clave quitaría y quitar el resto"),
    ),
    (Msg::DesignKeyFill, t("Clave/relleno")),
    (
        Msg::DesignKeyFillHover,
        t(
            "Recortar la capa con una imagen de clave aparte: donde la entrada de clave es blanca se ve la capa, donde es negra se quita",
        ),
    ),
    (
        Msg::DesignReasonNoKeyInput,
        t(
            "ninguna entrada puede ser la clave: añada una entrada de imagen que no muestre una escena",
        ),
    ),
    (Msg::DesignKeyInput, t("Entrada de clave")),
    (
        Msg::DesignKeyInputHover,
        t(
            "La entrada cuya imagen es la clave: su brillo, o su alfa, decide dónde se ve esta capa. Las entradas que muestran una escena no pueden ser clave",
        ),
    ),
    (
        Msg::DesignKeyInputChoiceHover,
        t("Usar esta entrada como imagen de clave"),
    ),
    (Msg::DesignKeyFillAlpha, t("Usar alfa de la clave")),
    (
        Msg::DesignKeyFillAlphaHover,
        t(
            "Tomar la clave del canal alfa de la entrada de clave en lugar de su brillo, para un gráfico con transparencia",
        ),
    ),
    (Msg::DesignKeyMatte, t("Mate")),
    (
        Msg::DesignKeyMatteHover,
        t(
            "Afinar el mate de la clave una vez extraído: recortarlo, encogerlo o ampliarlo, suavizar su borde y quitar el derrame de color",
        ),
    ),
    (Msg::DesignKeyClipBlack, t("Recorte de negro")),
    (
        Msg::DesignKeyClipBlackHover,
        t(
            "Los valores del mate iguales o inferiores a este pasan a ser totalmente transparentes, de 0 a 254 (siempre por debajo de Recorte de blanco)",
        ),
    ),
    (Msg::DesignKeyClipWhite, t("Recorte de blanco")),
    (
        Msg::DesignKeyClipWhiteHover,
        t(
            "Los valores del mate iguales o superiores a este pasan a ser totalmente opacos, de 1 a 255 (siempre por encima de Recorte de negro)",
        ),
    ),
    (Msg::DesignKeyChoke, t("Contracción")),
    (
        Msg::DesignKeyChokeHover,
        t("Encoger la zona conservada (positivo) o ampliarla (negativo) hasta 8 píxeles"),
    ),
    (Msg::DesignKeyEdgeSoftness, t("Suavidad del borde")),
    (
        Msg::DesignKeyEdgeSoftnessHover,
        t("Difuminar el borde del mate hasta 8 píxeles; 0 lo deja nítido"),
    ),
    (Msg::DesignKeyDespill, t("Supresión de derrame")),
    (
        Msg::DesignKeyDespillHover,
        t("Quitar de la imagen conservada el tinte de una pantalla verde o azul"),
    ),
    (Msg::DesignKeyDespillOff, t("Sin supresión")),
    (
        Msg::DesignKeyDespillOffHover,
        t("Dejar los colores conservados como están"),
    ),
    (
        Msg::DesignKeyDespillGreenHover,
        t("Quitar el derrame verde de la imagen conservada, para una pantalla verde"),
    ),
    (
        Msg::DesignKeyDespillBlueHover,
        t("Quitar el derrame azul de la imagen conservada, para una pantalla azul"),
    ),
    (Msg::DesignKeyDespillStrength, t("Intensidad de supresión")),
    (
        Msg::DesignKeyDespillStrengthHover,
        t("Cuánto derrame se quita, de 0 a 255"),
    ),
    (Msg::DesignKeyLightWrap, t("Envoltura de luz")),
    (
        Msg::DesignKeyLightWrapHover,
        t(
            "Deja que la imagen detrás de la capa brille en sus bordes, de 0 (apagado) a 255, para integrar al sujeto recortado en la escena",
        ),
    ),
    (Msg::DesignMatte, t("Máscara de recorte")),
    (
        Msg::DesignMatteHover,
        t(
            "Recorta la capa con una forma de 3 a 8 puntos, para quitar soportes, trípodes y bordes de la pantalla que el recorte no elimina",
        ),
    ),
    (Msg::DesignMatteOff, t("Sin máscara de recorte")),
    (
        Msg::DesignMatteOffHover,
        t("Conservar toda la capa recortada"),
    ),
    (Msg::DesignMatteInside, t("Conservar dentro")),
    (
        Msg::DesignMatteInsideHover,
        t("Conservar solo lo que queda dentro de la forma"),
    ),
    (Msg::DesignMatteOutside, t("Conservar fuera")),
    (
        Msg::DesignMatteOutsideHover,
        t("Quitar lo que queda dentro de la forma y conservar el resto"),
    ),
    (
        Msg::DesignMattePointHover,
        t(
            "Punto {number} de la máscara de recorte, a lo ancho (primero) y a lo alto (segundo) de la capa recortada en porcentaje; arrastra su punto numerado en el lienzo para moverlo",
        ),
    ),
    (Msg::DesignMatteAddPoint, t("Añadir punto")),
    (
        Msg::DesignMatteAddPointHover,
        t("Añadir un punto a mitad del borde que cierra la forma, como mucho 8 puntos"),
    ),
    (
        Msg::DesignMatteRemovePointHover,
        t("Quitar este punto; la forma conserva al menos 3"),
    ),
    (
        Msg::DesignMatteFlat,
        t("Sin cambios: los puntos quedarían todos en una línea"),
    ),
    (
        Msg::DesignMatteFlatHover,
        t(
            "Una máscara de recorte necesita un área; con todos los puntos en una línea quitaría la capa entera, así que se rechazó el cambio",
        ),
    ),
    (Msg::DesignKeyLightWrapWidth, t("Ancho de la envoltura")),
    (
        Msg::DesignKeyLightWrapWidthHover,
        t("Hasta dónde llega la luz del fondo en los bordes de la capa, de 1 a 8 píxeles"),
    ),
    (Msg::DesignGroupEffects, t("Efectos")),
    (
        Msg::DesignEffectsHover,
        t(
            "Efectos de vídeo de la capa, aplicados en el orden de la lista desde arriba, como mucho 8 por capa",
        ),
    ),
    (Msg::DesignEffectsEmpty, t("Sin efectos.")),
    (Msg::DesignEffectAdd, t("Añadir efecto")),
    (
        Msg::DesignEffectAddHover,
        t(
            "Añadir un efecto al final de la lista de la capa; empieza neutro y deja la imagen como está",
        ),
    ),
    (Msg::DesignEffectLevels, t("Niveles")),
    (
        Msg::DesignEffectLevelsHover,
        t(
            "Niveles: estirar el rango de brillo para que el nivel Negro pase a negro y el nivel Blanco a blanco",
        ),
    ),
    (Msg::DesignEffectBrightnessContrast, t("Brillo / contraste")),
    (
        Msg::DesignEffectBrightnessContrastHover,
        t("Brillo / contraste: aclarar u oscurecer la imagen y subir o bajar su contraste"),
    ),
    (
        Msg::DesignEffectSaturationHover,
        t("Saturación: colores más vivos o más grises, hasta blanco y negro"),
    ),
    (Msg::DesignEffectSharpen, t("Enfoque")),
    (
        Msg::DesignEffectSharpenHover,
        t("Enfoque: realzar los bordes y los detalles finos"),
    ),
    (Msg::DesignEffectFlip, t("Volteo")),
    (
        Msg::DesignEffectFlipHover,
        t("Volteo: reflejar la imagen en horizontal, en vertical o en ambos sentidos"),
    ),
    (
        Msg::DesignEffectBlackHover,
        t("Nivel de entrada que pasa a negro, de 0 a 254 (siempre por debajo de Blanco)"),
    ),
    (
        Msg::DesignEffectWhiteHover,
        t("Nivel de entrada que pasa a blanco, de 1 a 255 (siempre por encima de Negro)"),
    ),
    (Msg::DesignEffectBrightness, t("Brillo")),
    (
        Msg::DesignEffectBrightnessHover,
        t("Aclarar (positivo) u oscurecer (negativo) la imagen, de -100 a 100"),
    ),
    (
        Msg::DesignEffectContrastHover,
        t("Subir (positivo) o bajar (negativo) el contraste, de -100 a 100"),
    ),
    (Msg::DesignEffectAmount, t("Cantidad")),
    (
        Msg::DesignEffectSaturationAmountHover,
        t("Colores más vivos (positivo) o más grises (negativo); -100 es blanco y negro"),
    ),
    (
        Msg::DesignEffectSharpenAmountHover,
        t("Intensidad del enfoque, en porcentaje"),
    ),
    (Msg::DesignEffectHorizontal, t("Horizontal")),
    (
        Msg::DesignEffectHorizontalHover,
        t("Reflejar la imagen de izquierda a derecha"),
    ),
    (Msg::DesignEffectVertical, t("Vertical")),
    (
        Msg::DesignEffectVerticalHover,
        t("Reflejar la imagen de arriba abajo"),
    ),
    (
        Msg::DesignEffectUpHover,
        t("Aplicar este efecto un puesto antes en la lista"),
    ),
    (
        Msg::DesignEffectDownHover,
        t("Aplicar este efecto un puesto después en la lista"),
    ),
    (
        Msg::DesignEffectRemoveHover,
        t("Quitar este efecto de la capa"),
    ),
    (
        Msg::DesignReasonEffectsFull,
        t("la capa ya tiene 8 efectos, el máximo posible"),
    ),
    (
        Msg::DesignReasonEffectsDiffer,
        t(
            "las capas seleccionadas tienen efectos distintos; seleccione una sola capa para editarlos",
        ),
    ),
    (
        Msg::DesignReasonEffectFirst,
        t("este efecto ya es el primero"),
    ),
    (
        Msg::DesignReasonEffectLast,
        t("este efecto ya es el último"),
    ),
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
        t("Añada una entrada de archivo de medios, PDF o presentación para controlarla aquí."),
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
    (Msg::KindMediaListKey, t("LISTA DE MEDIOS")),
    (
        Msg::KindMediaListHover,
        t(
            "Una entrada de lista de medios: reproduce archivos de vídeo y audio uno tras otro, en orden o al azar",
        ),
    ),
    (Msg::NameMediaList, t("Lista de medios")),
    (Msg::MlFiles, t("Archivos")),
    (
        Msg::MlFilesHover,
        t(
            "Un archivo de vídeo o audio por línea, o una lista M3U (.m3u, .m3u8) cuyos archivos se añaden en su orden con sus títulos #EXTINF; cada archivo es una ruta absoluta en el ordenador del servidor (se copia al proyecto) o una URI asset:// que ya esté en él",
        ),
    ),
    (Msg::MlFilesCount, t("Elementos: {count}")),
    (Msg::MlAutoNext, t("Siguiente automático")),
    (
        Msg::MlAutoNextHover,
        t("Cuando un elemento termina, el siguiente se reproduce enseguida"),
    ),
    (Msg::MlAutoFirst, t("Volver al primero")),
    (
        Msg::MlAutoFirstHover,
        t("Cuando termina el último elemento, prepara el primero, detenido"),
    ),
    (Msg::MlLoop, t("Repetir la lista")),
    (
        Msg::MlLoopHover,
        t("Tras el último elemento, vuelve a reproducir la lista desde el primero"),
    ),
    (Msg::MlShuffle, t("Aleatorio")),
    (
        Msg::MlShuffleHover,
        t(
            "Reproduce los elementos en orden aleatorio; cada vuelta de una lista repetida se baraja de nuevo, igual en cada reproducción del registro",
        ),
    ),
    (Msg::MlReshuffle, t("Barajar de nuevo")),
    (
        Msg::MlReshuffleHover,
        t("Elige otro orden aleatorio para esta lista"),
    ),
    (
        Msg::MlTransitionHover,
        t(
            "Cómo pasa una lista en reproducción de un elemento al siguiente con Siguiente automático, Siguiente y Anterior: los dos elementos se mezclan durante los cuadros de la transición mientras el saliente sigue reproduciéndose, y con Siguiente automático el siguiente empieza esos cuadros antes de que termine el actual",
        ),
    ),
    (
        Msg::MlTransitionCutHover,
        t("El siguiente elemento reemplaza al actual de inmediato"),
    ),
    (
        Msg::MlTransitionFadeHover,
        t("El siguiente elemento se funde sobre el actual"),
    ),
    (Msg::MlTransitionDip, t("Fundido a negro")),
    (
        Msg::MlTransitionDipHover,
        t(
            "El elemento actual se funde a negro en la primera mitad y el siguiente aparece desde negro en la segunda",
        ),
    ),
    (
        Msg::MlTransitionWipeHover,
        t("El siguiente elemento entra sobre el actual desde el borde izquierdo"),
    ),
    (
        Msg::MlTransitionFramesHover,
        t("Cuántos cuadros dura la transición entre dos elementos"),
    ),
    (Msg::MlReasonNoItems, t("añada al menos un archivo")),
    (
        Msg::MlReasonNotMedia,
        t("una línea no es un archivo de vídeo o audio ni una lista M3U"),
    ),
    (
        Msg::MlReasonPathInvalid,
        t("usa rutas absolutas en el ordenador del servidor o URI asset://"),
    ),
    (
        Msg::MlReasonPlaylistUnreadable,
        t("no se puede leer una lista M3U en este ordenador"),
    ),
    (
        Msg::MlReasonTooManyItems,
        t("una lista de medios admite como máximo 256 archivos"),
    ),
    (
        Msg::MlReasonTooMuchText,
        t("las rutas y los títulos son demasiado largos para una lista"),
    ),
    (Msg::MlTitle, t("Elementos de la lista")),
    (Msg::MlPosition, t("Elemento {position} de {count}")),
    (
        Msg::MlPositionHover,
        t("El lugar del elemento activo en el orden de reproducción (aleatorio o no)"),
    ),
    (Msg::MlTotal, t("Total {duration}")),
    (Msg::MlTotalPartial, t("Al menos {duration}")),
    (
        Msg::MlTotalHover,
        t(
            "Cuánto dura la lista entera de una pasada: cada elemento desde su marca de entrada hasta su marca de salida, o hasta el final de su archivo si no tiene marca",
        ),
    ),
    (
        Msg::MlTotalPartialHover,
        plural(
            "Cuánto dura como mínimo la lista entera: el archivo de un elemento aún no se ha medido o no se puede leer, así que su duración no se cuenta",
            "Cuánto dura como mínimo la lista entera: los archivos de {count} elementos aún no se han medido o no se pueden leer, así que sus duraciones no se cuentan",
        ),
    ),
    (
        Msg::MlNextHover,
        t("Activa el elemento siguiente; se reproduce enseguida si la lista está en reproducción"),
    ),
    (
        Msg::MlPreviousHover,
        t("Activa el elemento anterior; se reproduce enseguida si la lista está en reproducción"),
    ),
    (Msg::MlSearchHint, t("Buscar elementos")),
    (
        Msg::MlSearchHover,
        t("Muestra solo los elementos cuyo título o archivo contiene este texto"),
    ),
    (
        Msg::MlNoMatches,
        t("Ningún elemento coincide con la búsqueda"),
    ),
    (
        Msg::MlDragHover,
        t(
            "Arrástrelo sobre otro elemento para moverlo allí; la lista se reproduce en este orden salvo en aleatorio",
        ),
    ),
    (Msg::MlActive, t("ACTUAL")),
    (
        Msg::MlActiveHover,
        t("El elemento que reproduce ahora esta entrada"),
    ),
    (
        Msg::MlThumbHover,
        t("La imagen de este elemento en su punto de entrada, tomada de su archivo"),
    ),
    (
        Msg::MlThumbSimHover,
        t("Una tarjeta sustituta: este servidor no decodifica archivos de medios"),
    ),
    (
        Msg::MlThumbPendingHover,
        t(
            "La imagen de este elemento aún no ha llegado; los archivos de audio muestran una tarjeta de onda",
        ),
    ),
    (Msg::MlRemoveHover, t("Quita este elemento de la lista")),
    (
        Msg::MlReasonOnlyItem,
        t("una lista de medios conserva al menos un elemento"),
    ),
    (Msg::MlUpHover, t("Adelanta este elemento un lugar")),
    (Msg::MlDownHover, t("Retrasa este elemento un lugar")),
    (Msg::MlReasonFirstItem, t("el elemento ya es el primero")),
    (Msg::MlReasonLastItem, t("el elemento ya es el último")),
    (
        Msg::MlActivateHover,
        t("Activa este elemento; se reproduce enseguida si la lista está en reproducción"),
    ),
    (Msg::MlReasonAlreadyActive, t("el elemento ya está activo")),
    (
        Msg::MlMarkInHover,
        t("Marca de entrada de este elemento: el primer fotograma que reproduce"),
    ),
    (
        Msg::MlMarkOutHover,
        t(
            "Marca de salida de este elemento: termina antes de este fotograma. Sin marca, se reproduce hasta el final del archivo y muestra la duración del archivo, o FIN mientras el servidor aún no ha medido el archivo (o no puede leerlo); escriba 0 para quitar la marca",
        ),
    ),
    (Msg::MlMarkOutEnd, t("FIN")),
    (
        Msg::MlAddPathsHover,
        t(
            "Archivos que añadir al final de la lista: rutas absolutas en el ordenador del servidor (se copian al proyecto) o URI asset://, en líneas separadas, o una lista M3U",
        ),
    ),
    (Msg::MlAdd, t("Añadir al final")),
    (
        Msg::MlAddHover,
        t("Añade los archivos escritos al final de la lista"),
    ),
    // Entradas de documento en la pestaña Medios (IN-014).
    (Msg::DocHeading, t("Documentos")),
    (
        Msg::DocIntro,
        t(
            "PDF y presentaciones, mostrados página a página. Pasar de página es un control de operador; la página en pantalla se guarda con el proyecto.",
        ),
    ),
    (Msg::DocStateLoading, t("CARGANDO")),
    (Msg::DocStateConverting, t("CONVIRTIENDO")),
    (
        Msg::DocStateHover,
        t(
            "Si el servidor puede mostrar este documento: una presentación se convierte primero a PDF, y si no puede convertirse se indica qué hacer",
        ),
    ),
    (
        Msg::DocTitleHover,
        t("El título guardado en el archivo del documento"),
    ),
    (
        Msg::DocMessageHover,
        t("Lo que el servidor informa sobre este documento"),
    ),
    (
        Msg::DocServerConverterMissing,
        t("instale LibreOffice en el servidor o exporte la presentación a PDF"),
    ),
    (
        Msg::DocServerKeynote,
        t("exporte la presentación de Keynote a PDF (Archivo > Exportar a > PDF)"),
    ),
    (Msg::DocPrevious, t("Página anterior")),
    (
        Msg::DocPreviousHover,
        t("Mostrar la página anterior en esta entrada"),
    ),
    (Msg::DocNext, t("Página siguiente")),
    (
        Msg::DocNextHover,
        t("Mostrar la página siguiente en esta entrada"),
    ),
    (
        Msg::DocFirstHover,
        t("Mostrar la primera página en esta entrada"),
    ),
    (
        Msg::DocLastHover,
        t("Mostrar la última página en esta entrada"),
    ),
    (Msg::DocPageCounter, t("Página {page} de {count}")),
    (Msg::DocPageUnknownCount, t("Página {page} de ?")),
    (
        Msg::DocPageCounterHover,
        t("La página que muestra esta entrada y el número de páginas del documento"),
    ),
    (
        Msg::DocThumbnailHover,
        t("Mostrar la página {page} en esta entrada"),
    ),
    (Msg::DocNotes, t("Notas del orador")),
    (
        Msg::DocNotesHover,
        t(
            "Las notas de la página en pantalla, leídas de la presentación; solo la consola las muestra",
        ),
    ),
    (Msg::DocNotesNone, t("Esta página no tiene notas.")),
    (Msg::DocNotesLoading, t("Cargando las notas…")),
    (
        Msg::DocNotesUnavailable,
        t("No se pueden leer las notas de esta página."),
    ),
    (
        Msg::DocReasonPagesUnknown,
        t("aún no se conocen las páginas del documento"),
    ),
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
        t(
            "Añadir una entrada por archivo a partir de una lista de rutas: archivos de medios, PDF y presentaciones",
        ),
    ),
    (Msg::LcCategoriesEllipsis, t("Categorías…")),
    (Msg::LcImportTitle, t("Importar archivos")),
    (
        Msg::LcImportIntro,
        t(
            "Pegue una ruta de archivo por línea: vídeos, imágenes, archivos solo de audio (WAV, MP3, AAC/M4A, FLAC, Ogg) para fondos musicales y ráfagas, o PDF y presentaciones. Cada archivo se convierte en una entrada con su nombre (una entrada de documento para un PDF o una presentación, una de medios en otro caso); el daemon copia los archivos del equipo al proyecto.",
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
        t(
            "Reproducir un archivo de medios (vídeo, o solo audio: un archivo de audio se ve negro y se oye por su canal); una ruta del equipo se copia al proyecto",
        ),
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
    (Msg::IdKindCueList, t("Lista de cues {id}")),
    (Msg::IdKindCue, t("Cue {id}")),
    (
        Msg::InUseCueAction,
        t("«{name}» aparece en el cue {cue} de la lista de cues {list}."),
    ),
    (Msg::AdviceCue, t("Quítela de ese cue primero.")),
    (
        Msg::RsnCueScheduled,
        t(
            "Las acciones de un cue están programadas para el fotograma {frame}; espere a que se ejecuten.",
        ),
    ),
    (
        Msg::RsnNoCueOnStandby,
        t("No hay ningún cue en espera; ponga uno en espera o reinicie la lista."),
    ),
    (
        Msg::RsnCueRunning,
        t("El cue {cue} sigue en marcha; deténgalo antes de cambiarlo."),
    ),
    (Msg::BoundsFieldCueLists, t("Listas de cues")),
    (Msg::BoundsFieldCues, t("Cues de una lista")),
    (Msg::BoundsFieldCueActions, t("Acciones de un cue")),
    (
        Msg::BoundsFieldCueListActions,
        t("Acciones de una lista de cues"),
    ),
    (
        Msg::BoundsFieldCueText,
        t("Texto de títulos en una lista de cues (bytes)"),
    ),
    (
        Msg::BoundsFieldTitleNumber,
        t("Magnitud de un número de título"),
    ),
    (Msg::IdKindScript, t("Guion {id}")),
    (Msg::IdKindShortcut, t("Atajo {id}")),
    (Msg::BoundsFieldScripts, t("Guiones de un proyecto")),
    (
        Msg::BoundsFieldScriptSource,
        t("Longitud del guion (bytes)"),
    ),
    (
        Msg::RsnScriptFailed,
        t(
            "El guion se detuvo con {failure} en la línea {line}, columna {column}; no se conservó nada de lo que hizo.",
        ),
    ),
    (Msg::ScriptFailSyntax, t("un error de sintaxis")),
    (Msg::ScriptFailRuntime, t("un error de ejecución")),
    (
        Msg::ScriptFailInstructions,
        t("su presupuesto de instrucciones agotado"),
    ),
    (Msg::ScriptFailActions, t("demasiadas acciones")),
    (Msg::ScriptFailVariables, t("demasiadas variables")),
    (Msg::ScriptFailStringBytes, t("una cadena demasiado larga")),
    (Msg::ScriptFailRefused, t("una acción rechazada")),
    (
        Msg::RsnScriptRunning,
        t("El guion {script} está en marcha; detenga primero su ejecución."),
    ),
    (Msg::ScriptFailWaits, t("demasiadas esperas")),
    (
        Msg::ScriptFailWaitFrames,
        t("esperas que suman más de diez minutos"),
    ),
    (
        Msg::ScriptRunWaiting,
        plural(
            "En marcha: la siguiente parte se ejecuta en el fotograma {frame} (espera {waits} de esta ejecución; sus esperas suman {count} fotograma hasta ahora, contando esta)",
            "En marcha: la siguiente parte se ejecuta en el fotograma {frame} (espera {waits} de esta ejecución; sus esperas suman {count} fotogramas hasta ahora, contando esta)",
        ),
    ),
    (
        Msg::ScriptRunWaitingHover,
        t(
            "La ejecución de este guion está en pausa en un wait. El servidor ejecuta el resto en ese fotograma, leyendo Program y Preview tal como estén entonces",
        ),
    ),
    (
        Msg::ScriptRunFaulted,
        t(
            "La ejecución falló en el fotograma {frame} con {failure} en la línea {line}, columna {column}; no se aplicó nada de ese fotograma.",
        ),
    ),
    (
        Msg::ScriptRunFaultedHover,
        t(
            "Una parte de la ejecución posterior a un wait falló al llegar su fotograma; la ejecución terminó ahí. El servidor indica dónde falló y por qué. Borre el informe o ejecute el guion de nuevo",
        ),
    ),
    (Msg::ScriptStop, t("Detener ejecución")),
    (
        Msg::ScriptStopHover,
        t(
            "Detener ahora la ejecución en pausa de este guion: no se ejecuta nada más de ella y lo que ya hizo se mantiene",
        ),
    ),
    (Msg::ScriptClearFault, t("Borrar ejecución fallida")),
    (
        Msg::ScriptClearFaultHover,
        t("Descartar el informe de la ejecución fallida en todas las consolas"),
    ),
    (
        Msg::ScriptReasonRunLimit,
        t("ya hay 8 ejecuciones de guion en pausa en un wait; detenga una primero"),
    ),
    (
        Msg::ScriptReasonThisRunning,
        t("este guion está en marcha; detenga primero su ejecución"),
    ),
    (Msg::ScriptListRunning, t("{name} (en marcha)")),
    (
        Msg::ScriptActiveRuns,
        t("Ejecuciones activas: {count} de {max} como máximo en pausa"),
    ),
    (
        Msg::ScriptActiveRunsHover,
        t(
            "Cada ejecución de guion en pausa en un wait y cada informe de ejecución fallida, tal como los ven todas las consolas. Varios guiones pueden ejecutarse a la vez; cada guion, una sola vez a la vez",
        ),
    ),
    (
        Msg::ScriptActiveRunWaiting,
        t(
            "{name}, ejecución {run}: en pausa en el wait de la línea {line} (espera {waits}); siguiente parte en el fotograma {frame}",
        ),
    ),
    (
        Msg::ScriptActiveRunFaulted,
        t("{name}, ejecución {run}: falló en el fotograma {frame}"),
    ),
    (
        Msg::ScriptActiveRunHover,
        t(
            "Una ejecución de guion: su guion, su número de ejecución en el servidor, dónde está en pausa y el fotograma en que se ejecuta su siguiente parte",
        ),
    ),
    (Msg::ScriptStopThisRun, t("Detener esta ejecución")),
    (
        Msg::ScriptStopThisRunHover,
        t(
            "Detener solo esta ejecución: no se ejecuta nada más de ella, lo que ya hizo se mantiene y las demás ejecuciones siguen",
        ),
    ),
    (Msg::ScriptClearThisFault, t("Borrar este informe")),
    (
        Msg::ScriptClearThisFaultHover,
        t("Descartar solo el informe de esta ejecución fallida, en todas las consolas"),
    ),
    (Msg::ScriptStopAll, t("Detener todas las ejecuciones")),
    (
        Msg::ScriptStopAllHover,
        t(
            "Detener todas las ejecuciones de guion en pausa y borrar todos los informes de ejecuciones fallidas, en todas las consolas; lo que las ejecuciones ya hicieron se mantiene",
        ),
    ),
    (Msg::MenuScripts, t("Guiones…")),
    (
        Msg::MenuScriptsHover,
        t(
            "Escriba y ejecute guiones del proyecto: pequeños programas de acciones de show que se ejecutan en el servidor de una vez",
        ),
    ),
    (Msg::ScriptsTitle, t("Guiones del proyecto")),
    (
        Msg::ScriptsIntro,
        t(
            "Un guion ejecuta acciones de show en el servidor. Cada parte hasta un wait se aplica en un mismo fotograma o no se aplica; tras un wait, el resto se ejecuta en un fotograma posterior. Los guiones se guardan con el proyecto y no pueden acceder a archivos, a la red ni a otros programas.",
        ),
    ),
    (Msg::ScriptsEmpty, t("Este proyecto aún no tiene guiones")),
    (Msg::ScriptListHover, t("Abrir {name} en el editor")),
    (Msg::ScriptNew, t("Nuevo guion")),
    (
        Msg::ScriptNewHover,
        t("Empieza un guion nuevo y vacío; se añade al proyecto al guardarlo"),
    ),
    (Msg::ScriptNameLabel, t("Nombre del guion")),
    (
        Msg::ScriptNameHover,
        t(
            "El nombre que se muestra en esta lista y que acepta la línea de comandos (de 1 a 64 bytes)",
        ),
    ),
    (
        Msg::ScriptSourceHover,
        t(
            "El texto del guion, una instrucción por línea; la línea con un error de sintaxis se marca en rojo mientras escribe",
        ),
    ),
    (Msg::ScriptSave, t("Guardar guion")),
    (
        Msg::ScriptSaveHover,
        t("Guarda el nombre y el texto en el proyecto como una edición que se puede deshacer"),
    ),
    (Msg::ScriptRevert, t("Descartar cambios del guion")),
    (
        Msg::ScriptRevertHover,
        t("Vuelve al guion tal como está guardado en el proyecto"),
    ),
    (Msg::ScriptRun, t("Ejecutar guion")),
    (
        Msg::ScriptRunHover,
        t(
            "Ejecuta ahora el guion guardado en el servidor; si falla un paso, no se conserva nada de su fotograma. La sesión necesita el permiso de cada acción que ejecuta (Preview, audio, texto de título); un guion con wait necesita el de todas las acciones que contiene",
        ),
    ),
    (Msg::ScriptRemove, t("Eliminar guion")),
    (
        Msg::ScriptRemoveHover,
        t("Quita el guion del proyecto como una edición que se puede deshacer"),
    ),
    (Msg::ScriptSyntaxOk, t("Sin errores de sintaxis")),
    (
        Msg::ScriptSyntaxError,
        t("Error de sintaxis en la línea {line}, columna {column}: {message}"),
    ),
    (
        Msg::ScriptLastRun,
        t("Última ejecución: {actions} en {instructions}, en el fotograma {frame}"),
    ),
    (
        Msg::ScriptLastFailure,
        t("La última ejecución se detuvo con {failure} en la línea {line}, columna {column}."),
    ),
    (
        Msg::ScriptActionCount,
        plural("{count} acción", "{count} acciones"),
    ),
    (
        Msg::ScriptInstructionCount,
        plural("{count} instrucción", "{count} instrucciones"),
    ),
    (Msg::ScriptFailureDetail, t("El servidor dijo: {detail}")),
    (
        Msg::ScriptLastRunByCue,
        t(
            "Última ejecución por la lista de cues {list}: {actions} en {instructions}, en el fotograma {frame}",
        ),
    ),
    (
        Msg::ScriptLastRunHover,
        t(
            "Lo que hizo la última ejecución de este script: una ejecución desde cualquier consola o un paso de script de una lista de cues. Una ejecución detenida solo se muestra en la consola que la pidió, hasta que el script vuelva a ejecutarse",
        ),
    ),
    (
        Msg::ScriptReference,
        t(
            "Instrucciones: let NOMBRE = VALOR · if … elif … else … end · repeat N … end · while … end · stop · wait FOTOGRAMAS (hasta diez minutos en total). Acciones: preview, cut, transition EFECTO, FOTOGRAMAS, stinger, overlay CANAL, ENTRADA, overlay_off, title_on, title_text, title_off, ftb, ftb_off, playlist_start, playlist_stop, gain, mute, unmute, record_start, record_stop, stream_start, stream_stop, play, pause, seek, restart. program y preview contienen los números de las entradas en el aire; una entrada es su número (1 es la primera) o su nombre entre comillas; # empieza un comentario.",
        ),
    ),
    (Msg::ScriptReasonUnsaved, t("guarde el guion primero")),
    (Msg::ScriptReasonUnchanged, t("el guion no ha cambiado")),
    (
        Msg::ScriptReasonName,
        t("escriba un nombre de 1 a 64 bytes"),
    ),
    (Msg::ScriptReasonLimit, t("el proyecto ya tiene 16 guiones")),
    (
        Msg::ScriptReasonTooLong,
        t("el guion supera los 4096 bytes o contiene caracteres de control"),
    ),
    (
        Msg::ScriptReasonSyntax,
        t("corrija primero el error de sintaxis"),
    ),
    (
        Msg::ScriptReasonNotSaved,
        t("el guion aún no está en el proyecto"),
    ),
    (
        Msg::ScriptReasonRunByCue,
        t("una lista de cues ejecuta este guion; quítelo antes de cada cue"),
    ),
    (Msg::TabCues, t("Cues")),
    (
        Msg::TabCuesHover,
        t(
            "Listas de cues del operador: arme cues con acciones del show y dispárelos en orden con GO",
        ),
    ),
    (Msg::ShortcutGroupCues, t("Lista de cues")),
    (
        Msg::ShortcutCueGo,
        t("GO: disparar el cue en espera de la lista del panel Cues"),
    ),
    (
        Msg::ShortcutCuePrevious,
        t("Mover la espera del panel Cues al cue anterior"),
    ),
    (
        Msg::ShortcutCueNext,
        t("Mover la espera del panel Cues al cue siguiente sin dispararlo"),
    ),
    (
        Msg::ShortcutCueArm,
        t("Armar GO: disparar el cue en espera del panel Cues tras su retardo «Disparar en»"),
    ),
    (
        Msg::ShortcutCueDisarm,
        t("Desarmar el GO armado del panel Cues"),
    ),
    (
        Msg::ShortcutCueStop,
        t(
            "Detener la lista del panel Cues: descartar las acciones del cue que espera y el GO armado",
        ),
    ),
    (
        Msg::ShortcutCueReset,
        t("Reiniciar la lista del panel Cues y poner su primer cue en espera"),
    ),
    (Msg::CueGo, t("GO")),
    (
        Msg::CueGoHover,
        t(
            "Disparar el cue en espera: sus acciones hasta la primera pausa se ejecutan en un mismo fotograma [G]",
        ),
    ),
    (Msg::CueGoNothing, t("Nada en espera")),
    (Msg::CueStandbyLabel, t("En espera: {name}")),
    (Msg::CueStateStandby, t("EN ESPERA")),
    (Msg::CueBack, t("Retroceder")),
    (Msg::CueBackHover, t("Mover la espera al cue anterior")),
    (Msg::CueSkip, t("Saltar")),
    (
        Msg::CueSkipHover,
        t("Pasar la espera al cue siguiente sin dispararlo"),
    ),
    (
        Msg::CueStopHover,
        t("Descartar las acciones pendientes del cue que espera y el GO armado"),
    ),
    (Msg::CueArmIn, t("Disparar en")),
    (
        Msg::CueArmDelayHover,
        t(
            "Cuánto después de armarlo se dispara el cue en espera: segundos, MM:SS o HH:MM:SS, de 1 segundo a 24 horas",
        ),
    ),
    (Msg::CueArm, t("Armar GO")),
    (
        Msg::CueArmHover,
        t(
            "Armar el cue en espera para que se dispare solo cuando pase el retardo de al lado, igual que GO; el reloj del servidor lo dispara aunque se cierre esta consola",
        ),
    ),
    (Msg::CueDisarm, t("Desarmar")),
    (
        Msg::CueDisarmHover,
        t("Cancelar el GO armado para que nunca se dispare"),
    ),
    (
        Msg::CueArmedNote,
        t("Armado: «{name}» se dispara en {time}, a las {clock} UTC"),
    ),
    (
        Msg::CueArmedDueNote,
        t("Armado: «{name}» vence ahora; esperando a que el servidor lo dispare"),
    ),
    (
        Msg::CueArmedOverdueNote,
        t(
            "Armado: «{name}» lleva {time} de retraso; el servidor aún no ha informado de que lo disparó",
        ),
    ),
    (
        Msg::CueArmedFrameNote,
        t("Armado: «{name}» se dispara en el fotograma de motor {frame}"),
    ),
    (
        Msg::CueArmedHover,
        t(
            "Vence en el fotograma de motor {frame}. Esta consola estima la cuenta atrás y la hora a partir del último fotograma que informó el servidor; el servidor dispara el cue en ese fotograma con su propio reloj, aunque esta consola se cierre.",
        ),
    ),
    (
        Msg::CueRunBoundNote,
        t("Vinculado en GO: Previo {preview} · Programa {program} · Seleccionada {selected}"),
    ),
    (
        Msg::CueRunBoundHover,
        t(
            "Las entradas a las que el último GO vinculó los papeles Previo, Programa y Seleccionada de esta lista. El resto de su ejecución, esperas y cues encadenados incluidos, actúa sobre estas entradas hasta el siguiente GO o Reiniciar",
        ),
    ),
    (
        Msg::CueArmedBoundNote,
        t("GO armado vinculado: Previo {preview} · Programa {program} · Seleccionada {selected}"),
    ),
    (
        Msg::CueArmedBoundHover,
        t(
            "Las entradas a las que el GO armado vinculó sus papeles Previo, Programa y Seleccionada al armarse; actúa sobre ellas cuando se dispara",
        ),
    ),
    (Msg::CueBoundNone, t("sin vincular")),
    (
        Msg::CueReasonArmDelay,
        t("escribe un retardo de 1 segundo a 24 horas en segundos, MM:SS o HH:MM:SS"),
    ),
    (
        Msg::CueReasonNotArmed,
        t("no hay ningún GO armado en esta lista"),
    ),
    (
        Msg::CueReasonArmed,
        t("hay un GO armado para este cue en esta lista; desármalo primero"),
    ),
    (Msg::CueReset, t("Reiniciar")),
    (
        Msg::CueResetHover,
        t("Borrar los cues disparados y poner el primero en espera"),
    ),
    (
        Msg::CueListPickHover,
        t("Elegir la lista de cues que muestra este panel y que dispara GO"),
    ),
    (Msg::CueListNew, t("Nueva lista")),
    (
        Msg::CueListNewHover,
        t("Crear una lista de cues vacía (se puede deshacer)"),
    ),
    (Msg::CueListDefaultName, t("Lista de cues {number}")),
    (Msg::CueListRemove, t("Eliminar lista")),
    (
        Msg::CueListRemoveHover,
        t("Eliminar esta lista de cues (se puede deshacer)"),
    ),
    (Msg::CueListNameLabel, t("Nombre de la lista")),
    (
        Msg::CueListNameHover,
        t("Nombre de esta lista de cues: hasta 64 bytes; pulse Intro para guardar"),
    ),
    (
        Msg::CueListsEmpty,
        t("Aún no hay listas de cues. Cree una para armar cues."),
    ),
    (Msg::CueListEmpty, t("Esta lista aún no tiene cues.")),
    (Msg::CueAdd, t("Añadir cue")),
    (
        Msg::CueAddHover,
        t("Añadir un cue nuevo y vacío al final de la lista"),
    ),
    (Msg::CueDefaultName, t("Cue {number}")),
    (
        Msg::CueNameHover,
        t("Nombre del cue: hasta 64 bytes; pulse Intro para guardar"),
    ),
    (Msg::CueFollow, t("Seguir")),
    (
        Msg::CueFollowHover,
        t(
            "Seguimiento automático: disparar el cue siguiente en cuanto se ejecute la última acción de este",
        ),
    ),
    (
        Msg::CueJumpHover,
        t("Poner este cue en espera para que GO lo dispare a continuación"),
    ),
    (
        Msg::CueFireHover,
        t("Disparar este cue ahora, esté donde esté la espera"),
    ),
    (Msg::CueUpHover, t("Subir este cue")),
    (Msg::CueDownHover, t("Bajar este cue")),
    (
        Msg::CueRemoveHover,
        t("Eliminar este cue (se puede deshacer)"),
    ),
    (
        Msg::CueExpandHover,
        t("Mostrar u ocultar las acciones de este cue"),
    ),
    (Msg::CueStatePlayed, t("DISPARADO")),
    (Msg::CueStateNotPlayed, t("EN COLA")),
    (Msg::CueStateFault, t("FALLO")),
    (
        Msg::CueStateHover,
        t(
            "Estado de este cue: en cola (aún sin disparar), al aire, en espera, armado para dispararse más tarde, esperando sus próximas acciones, disparado o detenido por una acción rechazada",
        ),
    ),
    (
        Msg::CueWaitingNote,
        t("Próximas acciones en el fotograma {frame}"),
    ),
    (
        Msg::CueFaultNote,
        t("El cue «{cue}» se detuvo en la acción {action}."),
    ),
    (
        Msg::CueTakeFollowNote,
        t(
            "Toma Programa en el mismo fotograma que el cue \"{name}\", que pasa a este automáticamente: añade una espera o desmarca su Seguir. No se guarda hasta corregirlo.",
        ),
    ),
    (
        Msg::CueTakeTwiceNote,
        t(
            "Toma Programa dos veces en un fotograma: añade una espera entre las tomas. No se guarda hasta corregirlo.",
        ),
    ),
    (Msg::CueActionAdd, t("Añadir acción")),
    (Msg::CueActionAddHover, t("Añadir una acción a este cue")),
    (Msg::CueActionKindHover, t("Qué hace esta acción")),
    (Msg::CueActionUpHover, t("Adelantar esta acción")),
    (Msg::CueActionDownHover, t("Atrasar esta acción")),
    (Msg::CueActionRemoveHover, t("Eliminar esta acción")),
    (Msg::CueActOverlayOn, t("Overlay dentro")),
    (Msg::CueActOverlayOff, t("Overlay fuera")),
    (Msg::CueActTitleOn, t("Título dentro")),
    (Msg::CueActTitleText, t("Texto de título")),
    (Msg::CueActTitleNumber, t("Número de título")),
    (Msg::CueActTitleOff, t("Título fuera")),
    (Msg::CueActPlaylistStart, t("Iniciar lista")),
    (Msg::CueActPlaylistStop, t("Detener lista")),
    (Msg::CueActGain, t("Fader")),
    (Msg::CueActMute, t("Silencio")),
    (Msg::CueActRecord, t("Grabar")),
    (Msg::CueActStream, t("Emitir")),
    (Msg::CueActWait, t("Esperar")),
    (Msg::CueActMediaPlay, t("Reproducir medio")),
    (Msg::CueActMediaPause, t("Pausar medio")),
    (Msg::CueActMediaSeek, t("Buscar en medio")),
    (Msg::CueActMediaRestart, t("Reiniciar medio")),
    (Msg::CueActDelay, t("Retardo")),
    (Msg::CueActRunScript, t("Guion")),
    (
        Msg::CueParamScriptHover,
        t(
            "El guion del proyecto que este paso ejecuta en su fotograma: todas sus acciones o ninguna, y si falla, falla el cue. Dispararlo requiere el permiso de cada acción que contiene el guion, en cualquier rama",
        ),
    ),
    (
        Msg::CueParamMediaHover,
        t("La entrada de medios cuyo transporte controla esta acción"),
    ),
    (
        Msg::CueParamSeekHover,
        t("Fotograma de origen al que saltar; se mantiene dentro del recorte del clip"),
    ),
    (
        Msg::CueParamDelayHover,
        t(
            "Milisegundos de espera antes de que se ejecuten las acciones restantes del cue; se redondean hacia arriba a fotogramas completos a la cadencia del programa",
        ),
    ),
    (Msg::CueMillisUnit, t("ms")),
    (Msg::CueParamPreview, t("(Previo)")),
    (Msg::CueParamInputHover, t("La entrada que usa esta acción")),
    (Msg::CueTargetPreview, t("Entrada en Previo")),
    (Msg::CueTargetProgram, t("Entrada en Programa")),
    (Msg::CueTargetSelected, t("Entrada seleccionada")),
    (
        Msg::CueTargetPreviewHover,
        t(
            "Actúa sobre la entrada que esté en Previo cuando se dispara la acción (en una cue, cuando se da su GO), no sobre una entrada fija",
        ),
    ),
    (
        Msg::CueTargetProgramHover,
        t(
            "Actúa sobre la entrada que esté en Programa cuando se dispara la acción (en una cue, cuando se da su GO), no sobre una entrada fija",
        ),
    ),
    (
        Msg::CueTargetSelectedHover,
        t(
            "Actúa sobre la entrada seleccionada en la consola que la dispara: el último mosaico de entrada pulsado allí. Se rechaza si esa consola no tiene ninguna entrada seleccionada",
        ),
    ),
    (Msg::CuePvUnboundTarget, t("{action}: {target}")),
    (
        Msg::CuePvRefNoSelection,
        t("no hay ninguna entrada seleccionada en esta consola"),
    ),
    (
        Msg::TileSelectedNote,
        t(
            "Seleccionada: los atajos y las cues dirigidos a la entrada seleccionada actúan sobre esta entrada",
        ),
    ),
    (
        Msg::CueParamEffectHover,
        t("Efecto de transición: Corte pasa al instante"),
    ),
    (Msg::CueParamFramesHover, t("Duración en fotogramas")),
    (Msg::CueParamChannelHover, t("Canal de overlay, del 1 al 8")),
    (
        Msg::CueParamFieldHover,
        t("El campo de texto del título que se fija"),
    ),
    (
        Msg::CueParamTextHover,
        t("Texto que se fija: hasta 128 bytes; pulse Intro para guardar"),
    ),
    (
        Msg::CueParamNumberFieldHover,
        t(
            "El campo numérico del título que se fija o se incrementa; también sirve un campo de texto con un número entero",
        ),
    ),
    (
        Msg::CueParamNumberModeHover,
        t(
            "Fijar en reemplaza el número del campo; Sumar le añade la cantidad (una cantidad negativa descuenta)",
        ),
    ),
    (
        Msg::CueParamNumberHover,
        t(
            "El número que se fija, o la cantidad distinta de cero que se suma: como máximo 999999999999 en valor absoluto",
        ),
    ),
    (Msg::CueNumberSet, t("Fijar en")),
    (Msg::CueNumberStep, t("Sumar")),
    (
        Msg::CueParamActiveHover,
        t("Marcado funde a negro; sin marcar vuelve de negro"),
    ),
    (Msg::CueParamGainHover, t("Nivel del fader en dB")),
    (
        Msg::CueParamMuteHover,
        t(
            "Silenciado acalla el canal, Sin silencio lo devuelve y Alternar silencio lo invierte según esté cuando la acción se ejecuta",
        ),
    ),
    (
        Msg::CueParamGainModeHover,
        t(
            "Fijar nivel pone el fader en un nivel; Mover lo desplaza desde donde esté cuando la acción se ejecuta; Desde el control sigue el valor del control OSC o fader que dispara el atajo",
        ),
    ),
    (
        Msg::CueParamGainStepHover,
        t(
            "Cuánto mover el fader, en dB: un valor positivo lo sube y uno negativo lo baja; el fader se detiene en sus extremos",
        ),
    ),
    (
        Msg::CueParamGainMinHover,
        t("El nivel que fija el control al fondo de su recorrido (0, o un valor OSC de 0.0)"),
    ),
    (
        Msg::CueParamGainMaxHover,
        t("El nivel que fija el control al tope de su recorrido (un valor OSC de 1.0)"),
    ),
    (Msg::CueGainSet, t("Fijar nivel")),
    (Msg::CueGainStep, t("Mover")),
    (Msg::CueGainFired, t("Desde el control")),
    (
        Msg::CueReasonFiredOnlyInShortcuts,
        t(
            "solo un atajo disparado desde un control OSC o un fader lleva un valor; un GO de cue no lleva ninguno",
        ),
    ),
    (
        Msg::UserShortcutReasonNeedsValue,
        t(
            "este atajo toma su ganancia del control que lo dispara: dispárelo con un mensaje OSC con un número decimal de 0 a 1",
        ),
    ),
    (
        Msg::UserShortcutLocalThenCapturing,
        t("Pulse ahora la segunda tecla del acorde de esta consola (Esc deja la tecla como está)"),
    ),
    (Msg::UserShortcutLocalChordLabel, t("Acorde de la consola")),
    (
        Msg::UserShortcutLocalSetThen,
        t("Añadir segunda tecla aquí"),
    ),
    (
        Msg::UserShortcutLocalSetThenHover,
        t(
            "Convierte la tecla de esta consola en un acorde: captura una segunda tecla pulsada después (Ctrl+K, luego 1), solo en esta consola. La segunda tecla puede ser cualquiera, incluso una que la consola usa sola",
        ),
    ),
    (
        Msg::UserShortcutLocalClearThen,
        t("Quitar segunda tecla aquí"),
    ),
    (
        Msg::UserShortcutLocalClearThenHover,
        t("Vuelve a dejar la tecla de esta consola como una sola tecla, sin su segunda tecla"),
    ),
    (Msg::UserShortcutLocalScopeLabel, t("Responde aquí en")),
    (
        Msg::UserShortcutLocalScopeHover,
        t(
            "Dónde dispara el atajo la tecla de esta consola: en cualquier parte de esta consola, solo mientras un panel tiene el foco, o solo mientras la ventana propia de un panel desacoplado tiene el foco. Las demás consolas conservan la tecla y el ámbito del proyecto",
        ),
    ),
    (
        Msg::UserShortcutReasonNoLocalKey,
        t("defina antes una tecla de consola: aquí el atajo responde a la tecla del proyecto"),
    ),
    (Msg::CueMuteOn, t("Silenciado")),
    (Msg::CueMuteOff, t("Sin silencio")),
    (Msg::CueMuteToggle, t("Alternar silencio")),
    (Msg::CueParamRecorderHover, t("Grabador 1 o 2")),
    (
        Msg::CueParamStartHover,
        t("Marcado inicia; sin marcar detiene"),
    ),
    (Msg::CueParamTargetHover, t("El destino de emisión")),
    (
        Msg::CueParamWaitHover,
        t("Fotogramas de espera antes de que se ejecuten las acciones restantes del cue"),
    ),
    (Msg::CueParamOn, t("Activo")),
    (Msg::CueFramesUnit, t("fr")),
    (
        Msg::CueViewOnly,
        t("Solo un operador con permiso de transición puede disparar cues"),
    ),
    (
        Msg::CueReasonRunning,
        t("un cue de esta lista espera sus próximas acciones; deténgalo primero"),
    ),
    (
        Msg::CueReasonListsFull,
        t("el show ya tiene el máximo de 8 listas de cues"),
    ),
    (
        Msg::CueReasonCuesFull,
        t("esta lista ya tiene el máximo de 64 cues"),
    ),
    (
        Msg::CueReasonActionsFull,
        t("este cue ya tiene el máximo de 16 acciones"),
    ),
    (Msg::CueReasonFirst, t("ya es el primero")),
    (Msg::CueReasonLast, t("ya es el último")),
    (
        Msg::CueReasonNothingToName,
        t("el show no tiene nada que esta acción pueda nombrar"),
    ),
    (Msg::CueReasonNoStandby, t("no hay ningún cue en espera")),
    (
        Msg::CueReasonNoScriptsLeft,
        t("esta lista ya ejecuta el máximo de 8 guiones"),
    ),
    (
        Msg::CueReasonChainPreview,
        t(
            "un cue que esto ejecuta selecciona Preview, lo que requiere el permiso select_preview que esta sesión no tiene",
        ),
    ),
    (
        Msg::CueReasonChainAudio,
        t(
            "un cue que esto ejecuta cambia el audio, lo que requiere el permiso control_audio que esta sesión no tiene",
        ),
    ),
    (
        Msg::CueReasonChainTitle,
        t(
            "un cue que esto ejecuta cambia el texto o los números de un título, lo que requiere el permiso edit_project que esta sesión no tiene",
        ),
    ),
    // What a cue does when fired (RC-017).
    (Msg::CuePvGoTitle, t("GO hará:")),
    (Msg::CuePvFireTitle, t("Disparar «{name}» hará:")),
    (
        Msg::CuePvHover,
        t(
            "Lo que hace el cue en espera al dispararse, calculado en esta consola con el programa tal como está. Los valores tras una espera o un script suponen que nada más cambia entretanto.",
        ),
    ),
    (
        Msg::CuePvChipHint,
        t("Pasa el puntero por el indicador de estado de un cue para ver qué hace al dispararlo."),
    ),
    (Msg::CuePvEmpty, t("Nada: este cue no tiene acciones.")),
    (
        Msg::CuePvMore,
        plural("…y {count} línea más", "…y {count} líneas más"),
    ),
    (
        Msg::CuePvSummaryRefused,
        t("Tal como está el programa, el servidor rechazaría este GO y no se ejecutaría nada."),
    ),
    (
        Msg::CuePvSummaryFault,
        t(
            "Tal como está el programa, la lista se detendría con un fallo en el paso marcado tras una espera.",
        ),
    ),
    (
        Msg::CuePvSummaryDenied,
        t(
            "Esta sesión no puede ejecutar todos los pasos marcados, así que el servidor rechazaría este GO.",
        ),
    ),
    (Msg::CuePvPvw, t("Ver en PVW")),
    (
        Msg::CuePvPvwHover,
        t(
            "Poner ahora en el bus de Previo la entrada que este cue lleva a Programa, sin dispararlo, para revisar el plano antes",
        ),
    ),
    (
        Msg::CuePvReasonNoBus,
        t("este cue no pone ninguna entrada conocida en Programa ni en Previo"),
    ),
    (
        Msg::CuePvReasonOnPreview,
        t("la entrada del cue ya está en Previo"),
    ),
    (
        Msg::CuePvReasonOnProgram,
        t("la entrada del cue ya está en Programa"),
    ),
    (Msg::CuePvNow, t("(ahora {value})")),
    (Msg::CuePvQuoted, t("«{text}»")),
    (Msg::CuePvPreviewInput, t("lo que haya en Previo")),
    (Msg::CuePvCut, t("Corte a {input}")),
    (
        Msg::CuePvMix,
        t("{effect} a {input} en {frames} fotogramas"),
    ),
    (Msg::CuePvPreview, t("Poner {input} en Previo")),
    (Msg::CuePvOverlayOn, t("Overlay {channel}: mostrar {input}")),
    (
        Msg::CuePvOverlayOff,
        t("Overlay {channel}: quitar del aire"),
    ),
    (Msg::CuePvOverlayEmpty, t("nada")),
    (
        Msg::CuePvTitleOn,
        t("Overlay {channel}: sacar el título {title}"),
    ),
    (Msg::CuePvTitleOff, t("Quitar del aire el título {title}")),
    (Msg::CuePvTitleSet, t("Poner {title} {field} a {value}")),
    (
        Msg::CuePvTitleStep,
        t("Sumar {step} a {title} {field}, hasta {value}"),
    ),
    (Msg::CuePvTitleStepBy, t("Sumar {step} a {title} {field}")),
    (Msg::CuePvFtbOn, t("Fundido a negro en {frames} fotogramas")),
    (
        Msg::CuePvFtbOff,
        t("Salir del negro en {frames} fotogramas"),
    ),
    (
        Msg::CuePvPlaylistStart,
        t("Iniciar la lista de reproducción (toma Programa)"),
    ),
    (
        Msg::CuePvPlaylistStop,
        t("Detener la lista de reproducción"),
    ),
    (Msg::CuePvGain, t("Poner el fader de {input} a {gain}")),
    (Msg::CuePvMute, t("Silenciar {input}")),
    (Msg::CuePvUnmute, t("Quitar el silencio de {input}")),
    (Msg::CuePvGainStep, t("Mover el fader de {input} {gain}")),
    (
        Msg::CuePvGainFired,
        t("Poner el fader de {input} según el control que dispara, de {min} a {max}"),
    ),
    (Msg::CuePvMuteToggle, t("Alternar el silencio de {input}")),
    (Msg::CuePvRecordStart, t("Empezar a grabar en REC {slot}")),
    (Msg::CuePvRecordStop, t("Dejar de grabar en REC {slot}")),
    (Msg::CuePvStreamStart, t("Empezar a emitir a {target}")),
    (Msg::CuePvStreamStop, t("Dejar de emitir a {target}")),
    (Msg::CuePvPlay, t("Reproducir {input}")),
    (Msg::CuePvPause, t("Pausar {input}")),
    (Msg::CuePvSeek, t("Llevar {input} al fotograma {frame}")),
    (
        Msg::CuePvRestart,
        t("Reiniciar {input} desde su primer fotograma"),
    ),
    (
        Msg::CuePvWaitFrames,
        plural(
            "Esperar {count} fotograma ({time}) y después:",
            "Esperar {count} fotogramas ({time}) y después:",
        ),
    ),
    (Msg::CuePvWaitTime, t("Esperar {time} y después:")),
    (Msg::CuePvCueEnds, t("El cue termina")),
    (Msg::CuePvScript, t("Ejecutar el script {name}")),
    (Msg::CuePvFollow, t("Después sigue el cue {name}:")),
    (
        Msg::CuePvAlreadyProgram,
        t("ya está en Programa: esta toma no hace nada"),
    ),
    (
        Msg::CuePvAlreadyPreview,
        t("ya está en Previo: sin cambios"),
    ),
    (Msg::CuePvAlreadyState, t("sin cambios: ya está así")),
    (
        Msg::CuePvNoStrip,
        t("esta entrada no tiene canal de audio: no pasa nada"),
    ),
    (Msg::CuePvDenied, t("Sin permiso: {reason}")),
    (Msg::CuePvRefused, t("Se rechazaría: {reason}")),
    (
        Msg::CuePvRefMissingInput,
        t("el programa no tiene esa entrada"),
    ),
    (Msg::CuePvRefNotMedia, t("no es una entrada multimedia")),
    (Msg::CuePvRefNotTitle, t("no es un título")),
    (
        Msg::CuePvRefImageField,
        t("ese campo contiene una imagen o un color, no un número"),
    ),
    (
        Msg::CuePvRefNotWhole,
        t("el campo no contiene un número entero"),
    ),
    (
        Msg::CuePvRefBound,
        t("el resultado pasaría de {max} en un sentido u otro"),
    ),
    (Msg::CuePvRefNoScript, t("el proyecto no tiene ese script")),
    (
        Msg::CuePvRefScriptWaits,
        t("el script contiene una espera, que un paso de cue no puede ejecutar"),
    ),
    (
        Msg::CuePvRefScriptSyntax,
        t("el script tiene un error de sintaxis, así que su ejecución falla enseguida"),
    ),
    (
        Msg::CuePvRefNoStream,
        t("el programa no tiene ese destino de emisión"),
    ),
    (
        Msg::CuePvRefNoStinger,
        t("la ranura de Stinger {slot} no tiene ningún stinger"),
    ),
    (
        Msg::CuePvRefPreviewIsProgram,
        t("esa entrada está en Programa, y Previo y Programa deben ser distintos"),
    ),
    (
        Msg::CuePvRefTwice,
        t("Programa se tomaría dos veces en un mismo fotograma"),
    ),
    (
        Msg::CuePvRefInFlight,
        t("la transición iniciada antes aún estaría en curso"),
    ),
    (
        Msg::CuePvRefTimedInFlight,
        t(
            "una transición temporizada sigue en curso en Programa; un GO ahora se rechazaría hasta que termine",
        ),
    ),
    (
        Msg::CuePvStingerNotReady,
        t(
            "la ranura de Stinger {slot} aún no está cargada, así que la toma puede usar su alternativa o ser rechazada",
        ),
    ),
    // Stream destinations (OR-006).
    (Msg::StreamStartAllKey, t("INICIAR TODO")),
    (Msg::StreamStopAllKey, t("DETENER TODO")),
    (
        Msg::StreamStartAllHover,
        t("Iniciar todos los destinos que no están en marcha. La grabación no se ve afectada."),
    ),
    (
        Msg::StreamStopAllHover,
        t("Detener todos los destinos en marcha. La grabación no se ve afectada."),
    ),
    (Msg::StreamAddKey, t("AÑADIR…")),
    (
        Msg::StreamAddHover,
        t("Añadir un destino de transmisión: RTMP, RTMPS, SRT o HLS local (como máximo {limit})"),
    ),
    (Msg::StreamRetryKey, t("REINTENTAR")),
    (
        Msg::StreamRetryHover,
        t("Reconectar «{name}» ahora en lugar de esperar al próximo reintento"),
    ),
    (Msg::StreamEditKey, t("EDITAR…")),
    (Msg::StreamEditHover, t("Cambiar los ajustes de «{name}»")),
    (
        Msg::StreamRemoveHover,
        t("Quitar el destino «{name}» del show"),
    ),
    (Msg::StreamRemoveName, t("Quitar destino")),
    (Msg::StreamRemoveMenuItem, t("Quitar destino…")),
    (
        Msg::StreamRowMoreHover,
        t("Más acciones para «{name}»: reintentar, editar, quitar"),
    ),
    (Msg::StreamStatState, t("Estado: {state}")),
    (Msg::StreamStatUrl, t("Destino: {url}")),
    (Msg::StreamStatBackup, t("Respaldo: {url}")),
    (
        Msg::StreamStatOutput,
        t("Salida: {output} · objetivo {target} kb/s"),
    ),
    (
        Msg::StreamStatKeyStored,
        t("Clave de transmisión: guardada (nunca se muestra)"),
    ),
    (
        Msg::StreamStatKeyMissing,
        t("Clave de transmisión: ninguna guardada"),
    ),
    (
        Msg::StreamStatLive,
        t(
            "Tasa {kbps} kb/s · {dropped} fotogramas descartados · {reconnects} reconexiones · en marcha {uptime}",
        ),
    ),
    (
        Msg::ReasonStreamNotStarted,
        t("el destino no está iniciado; Iniciar lo conecta"),
    ),
    (
        Msg::ReasonStreamRunning,
        t("detenga primero el destino; un destino en marcha no se puede cambiar"),
    ),
    (
        Msg::ReasonStreamInCue,
        t("una lista de cues inicia o detiene este destino; quite primero esas acciones"),
    ),
    (
        Msg::ReasonStreamNoConfig,
        t("los ajustes de este destino aún no están sincronizados"),
    ),
    (
        Msg::ReasonStreamTargetsFull,
        t("el show ya tiene el máximo de 5 destinos"),
    ),
    (
        Msg::ReasonAllStreamsStarted,
        t("todos los destinos ya están iniciados"),
    ),
    (
        Msg::ReasonNoStreamsStarted,
        t("ningún destino está iniciado"),
    ),
    (
        Msg::ReasonStreamGroupPending,
        t("un Iniciar todo o Detener todo anterior sigue pendiente"),
    ),
    (
        Msg::ReasonStreamNoOutputs,
        t("el show no tiene ninguna salida que publicar"),
    ),
    (
        Msg::ReasonHlsNoBackup,
        t("el HLS local no tiene servidor de respaldo"),
    ),
    (
        Msg::ReasonHlsNoKey,
        t("el HLS local no usa clave de transmisión"),
    ),
    (Msg::ReasonStreamUnchanged, t("aún no ha cambiado nada")),
    (Msg::StreamDialogAddTitle, t("Añadir destino")),
    (Msg::StreamDialogEditTitle, t("Editar destino")),
    (
        Msg::StreamDialogIntro,
        t(
            "Un destino publica una salida en un servicio de streaming, un receptor SRT o segmentos HLS locales. Cada destino se inicia, se detiene y se reconecta por su cuenta.",
        ),
    ),
    (Msg::StreamSectionDestination, t("Destino")),
    (Msg::StreamSectionVideo, t("Salida y vídeo")),
    (Msg::StreamSectionReconnect, t("Reconexión")),
    (
        Msg::StreamFieldNameHover,
        t(
            "Cómo nombran este destino la consola y las listas de cues: único, como máximo 128 bytes",
        ),
    ),
    (Msg::StreamFieldProtocol, t("Protocolo")),
    (
        Msg::StreamProtoRtmpHover,
        t("Publicar en un servidor de ingesta RTMP (rtmp://)"),
    ),
    (
        Msg::StreamProtoRtmpsHover,
        t("Publicar por RTMP cifrado con TLS (rtmps://), como prefieren la mayoría de plataformas"),
    ),
    (
        Msg::StreamProtoSrtHover,
        t("Enviar a un receptor SRT en host:puerto (srt://)"),
    ),
    (
        Msg::StreamProtoHlsHover,
        t("Escribir segmentos HLS y una lista de reproducción en un directorio local; sin clave"),
    ),
    (Msg::StreamFieldEndpoint, t("Servidor")),
    (Msg::StreamFieldDirectory, t("Directorio")),
    (
        Msg::StreamEndpointRtmpHover,
        t(
            "Host, puerto opcional y ruta de aplicación, sin el esquema ni la clave: live.example.com/app",
        ),
    ),
    (
        Msg::StreamEndpointSrtHover,
        t("Host y puerto opcional, sin el esquema: srt.example.com:9000"),
    ),
    (
        Msg::StreamEndpointHlsHover,
        t("Un directorio absoluto para los segmentos y la lista: /var/www/hls/show"),
    ),
    (Msg::StreamFieldBackup, t("Servidor de respaldo")),
    (
        Msg::StreamFieldBackupHover,
        t(
            "Segundo servidor opcional al que el destino conmuta, escrito como el servidor; déjelo vacío si no hay",
        ),
    ),
    (
        Msg::StreamFieldKeyHover,
        t(
            "La clave secreta que le da su servicio. Se guarda con el proyecto y nunca se vuelve a mostrar",
        ),
    ),
    (
        Msg::StreamKeyKeepHint,
        t("guardada — déjela vacía para conservarla"),
    ),
    (
        Msg::StreamKeyKeepCaption,
        t("Hay una clave guardada; escriba otra solo para sustituirla"),
    ),
    (
        Msg::StreamKeyClearCaption,
        t("La clave guardada se quita: el HLS local no usa ninguna"),
    ),
    (
        Msg::StreamKeyReplaceCaption,
        t("La clave escrita sustituye a la guardada; vacíe el campo para conservarla"),
    ),
    (
        Msg::StreamFieldOutputHover,
        t("Publicar la imagen y el sonido de esta salida"),
    ),
    (Msg::StreamFieldBitrate, t("Tasa de vídeo (kb/s)")),
    (
        Msg::StreamFieldBitrateHover,
        t("Tasa de vídeo del codificador, de 1000 a 100000 kb/s"),
    ),
    (Msg::StreamFieldStartup, t("Iniciar al arrancar")),
    (
        Msg::StreamFieldStartupHover,
        t(
            "Al arrancar el servidor, reanudar este destino si estaba en marcha; sin marcar, siempre arranca detenido",
        ),
    ),
    (
        Msg::StreamFieldFirstDelayHover,
        t(
            "Espera antes de la primera reconexión; cada espera siguiente se duplica. De 100 a 300000 ms",
        ),
    ),
    (Msg::StreamFieldMaxDelay, t("Espera máxima (ms)")),
    (
        Msg::StreamFieldMaxDelayHover,
        t("La espera más larga entre reconexiones, al menos la primera. Hasta 300000 ms"),
    ),
    (Msg::StreamFieldAttempts, t("Intentos")),
    (
        Msg::StreamFieldAttemptsHover,
        t("Intentos de reconexión antes de que el destino se rinda, de 1 a 10000"),
    ),
    (Msg::StreamFieldUnlimited, t("Ilimitados")),
    (
        Msg::StreamFieldUnlimitedHover,
        t("No dejar nunca de reconectar mientras el destino esté iniciado"),
    ),
    (
        Msg::ReasonStreamUnlimited,
        t("los intentos son ilimitados; desmarque Ilimitados para fijar un número"),
    ),
    (Msg::StreamDialogAddKey, t("AÑADIR")),
    (
        Msg::StreamDialogAddKeyHover,
        t("Añadir este destino al show"),
    ),
    (
        Msg::StreamDialogSaveKeyHover,
        t("Guardar los cambios de este destino"),
    ),
    (Msg::StreamDialogCancelHover, t("Cerrar sin guardar")),
    (
        Msg::StreamDialogNote,
        t("Deshacer revierte este cambio; la clave nunca se muestra"),
    ),
    (Msg::StreamErrNameEmpty, t("escriba un nombre")),
    (Msg::StreamErrNameLong, t("use como máximo 128 bytes")),
    (
        Msg::StreamErrNameTaken,
        t("otro destino ya tiene este nombre"),
    ),
    (Msg::StreamErrEndpointEmpty, t("escriba dónde publicar")),
    (Msg::StreamErrEndpointLong, t("use como máximo 1024 bytes")),
    (
        Msg::StreamErrEndpointChars,
        t("use caracteres imprimibles sin espacios"),
    ),
    (
        Msg::StreamErrEndpointScheme,
        t("omita el esquema; lo eligen las teclas de protocolo"),
    ),
    (
        Msg::StreamErrEndpointQuery,
        t("omita ? y #; ponga los tokens en la clave"),
    ),
    (
        Msg::StreamErrEndpointApp,
        t("añada la ruta de aplicación: host/app"),
    ),
    (Msg::StreamErrEndpointHost, t("indique un host")),
    (
        Msg::StreamErrEndpointCredentials,
        t("omita usuario:contraseña@; los secretos van en la clave"),
    ),
    (
        Msg::StreamErrEndpointSrtPath,
        t("SRT solo admite host[:puerto], sin ruta"),
    ),
    (
        Msg::StreamErrEndpointAbsolute,
        t("use un directorio absoluto que empiece por /"),
    ),
    (
        Msg::StreamErrEndpointSegments,
        t("quite los segmentos de ruta vacíos, . o .."),
    ),
    (
        Msg::StreamErrKeyRequired,
        t("escriba la clave de transmisión"),
    ),
    (
        Msg::StreamErrKeyChars,
        t("use de 4 a 512 caracteres imprimibles, sin espacios ni /"),
    ),
    (
        Msg::StreamErrKeySrt,
        t("una clave SRT no puede contener & ni ?"),
    ),
    (Msg::StreamErrBitrate, t("use de 1000 a 100000 kb/s")),
    (
        Msg::StreamErrDelays,
        t("la espera máxima debe ser al menos la primera, de 100 a 300000 ms"),
    ),
    (Msg::StreamErrAttempts, t("use de 1 a 10000 intentos")),
    (Msg::StreamErrOutput, t("elija una salida")),
    (Msg::StreamRemoveQuestion, t("¿Quitar «{name}» ({url})?")),
    (
        Msg::StreamRemoveWarning,
        t(
            "Sus ajustes y su clave guardada se van con él. La grabación y los demás destinos no se ven afectados.",
        ),
    ),
    (Msg::StreamRemoveKeyHover, t("Quitar este destino")),
    (Msg::StreamRemoveCancelHover, t("Conservar este destino")),
    (Msg::StreamFieldEncrypt, t("Cifrar")),
    (
        Msg::StreamFieldEncryptHover,
        t("Cifrar la señal SRT con AES; el receptor necesita la misma frase de cifrado"),
    ),
    (
        Msg::StreamFieldPassphraseHover,
        t(
            "La frase de cifrado SRT que también usa el receptor, de 10 a 79 caracteres imprimibles sin espacios. Se guarda con el proyecto y no vuelve a mostrarse",
        ),
    ),
    (
        Msg::StreamPassphraseKeepCaption,
        t("Hay una frase guardada; escriba otra solo para reemplazarla"),
    ),
    (
        Msg::StreamPassphraseReplaceCaption,
        t("La frase escrita reemplaza a la guardada; vacíe el campo para conservarla"),
    ),
    (
        Msg::StreamPassphraseClearCaption,
        t("La frase guardada se elimina: la señal se envía sin cifrar"),
    ),
    (
        Msg::ReasonStreamNotEncrypted,
        t("marque Cifrar para indicar una frase"),
    ),
    (
        Msg::StreamErrPassphraseRequired,
        t("escriba la frase de cifrado"),
    ),
    (
        Msg::StreamErrPassphraseChars,
        t("use de 10 a 79 caracteres imprimibles, sin espacios"),
    ),
    (Msg::StreamErrLatency, t("use de 20 a 8000 ms")),
    (Msg::NameSystemAudio, t("Audio del sistema")),
    (Msg::KindSystemAudioKey, t("AUDIO DEL SISTEMA")),
    (
        Msg::KindSystemAudioHover,
        t(
            "Una entrada solo de audio que escucha el sonido del propio servidor: un monitor de PulseAudio o PipeWire de lo que reproduce una salida, o un dispositivo de captura ALSA como un loopback; se vuelve a abrir automáticamente si el dispositivo desaparece",
        ),
    ),
    (Msg::SaBackend, t("Sistema de audio")),
    (Msg::SaBackendPulse, t("PulseAudio / PipeWire")),
    (
        Msg::SaBackendPulseHover,
        t(
            "Captura una fuente de PulseAudio o PipeWire del servidor; el monitor de una salida escucha lo que esa salida reproduce",
        ),
    ),
    (Msg::SaBackendAlsa, t("ALSA")),
    (
        Msg::SaBackendAlsaHover,
        t(
            "Captura un PCM de ALSA del servidor, como el lado de captura de una tarjeta loopback snd-aloop",
        ),
    ),
    (Msg::SaDevice, t("Dispositivo de origen")),
    (
        Msg::SaDeviceHover,
        t(
            "El nombre del dispositivo tal como lo conoce el sistema de audio del servidor; elegirlo en la lista de arriba lo rellena, y puede escribir un nombre que la lista no muestre (de 1 a 128 caracteres imprimibles)",
        ),
    ),
    (
        Msg::SaDevicePulseCaption,
        t(
            "@DEFAULT_MONITOR@ escucha la salida predeterminada; o indique una fuente como <sink>.monitor",
        ),
    ),
    (
        Msg::SaDeviceAlsaCaption,
        t("Por ejemplo hw:Loopback,1 (lado de captura de snd-aloop) o plughw:1,0"),
    ),
    (
        Msg::SaReasonDeviceEmpty,
        t("escriba el dispositivo que se captura"),
    ),
    (
        Msg::SaReasonDeviceInvalid,
        t("use de 1 a 128 caracteres imprimibles, sin empezar por - ni por un espacio"),
    ),
    (Msg::SaBadgeOpening, t("ABRIENDO")),
    (Msg::SaBadgeReopening, t("REABRIENDO {attempt}")),
    (
        Msg::SaDetail,
        t("Audio del sistema {device} ({backend}): {state}"),
    ),
    (
        Msg::SaDetailReopens,
        plural("reabierto {count} vez", "reabierto {count} veces"),
    ),
    // Lista de dispositivos de audio del sistema (AU-011, Protocolo 2.68).
    (Msg::SaRefresh, t("Actualizar lista")),
    (
        Msg::SaRefreshHover,
        t(
            "Pide al servidor que vuelva a listar sus dispositivos de captura de audio, por ejemplo tras conectar un micrófono USB o cargar una tarjeta loopback",
        ),
    ),
    (
        Msg::SaPickerHover,
        t(
            "Elija uno de los dispositivos de captura de audio que lista el servidor; el campo de nombre de abajo sigue su elección",
        ),
    ),
    (Msg::SaPickDefault, t("Dispositivo predeterminado")),
    (
        Msg::SaPickDefaultPulseHover,
        t(
            "Captura lo que reproduce la salida predeterminada del servidor ({device}), siguiendo la predeterminada cuando cambia",
        ),
    ),
    (
        Msg::SaPickDefaultAlsaHover,
        t("Captura el PCM predeterminado de ALSA en el servidor ({device})"),
    ),
    (Msg::SaPickTyped, t("Nombre escrito: {device}")),
    (
        Msg::SaDeviceOptionHover,
        t("Captura el dispositivo que el servidor llama {device}"),
    ),
    (
        Msg::SaTagChannels,
        plural("{count} canal", "{count} canales"),
    ),
    (Msg::SaTagMonitor, t("monitor de salida")),
    (Msg::SaTagDefault, t("predeterminado del sistema")),
    (
        Msg::SaListUnknown,
        t("Pidiendo al servidor sus dispositivos de captura de audio…"),
    ),
    (
        Msg::SaListUnsupported,
        t("Este servidor no captura audio del sistema, así que no lista dispositivos"),
    ),
    (Msg::SaListError, t("No se pudo listar {backend}: {error}")),
    (
        Msg::SaListEmpty,
        t(
            "El servidor no lista dispositivos de captura de {backend}; escriba el nombre del dispositivo",
        ),
    ),
    (
        Msg::SaListCount,
        plural(
            "El servidor lista {count} dispositivo de captura de {backend}",
            "El servidor lista {count} dispositivos de captura de {backend}",
        ),
    ),
    (Msg::MenuLiveLan, t("Espectadores LiveLAN…")),
    (
        Msg::MenuLiveLanHover,
        t(
            "Permita que quien esté en la red local vea Programa o una salida en un navegador, con PIN opcional",
        ),
    ),
    (Msg::LiveLanTitle, t("Espectadores LiveLAN")),
    (
        Msg::LiveLanIntro,
        t(
            "Los espectadores de esta red abren la dirección de abajo en un navegador (Safari, o Chrome en Android) para mirar. Los ajustes se guardan con el proyecto; el servidor debe ejecutarse con --live-lan-listen.",
        ),
    ),
    (
        Msg::LiveLanStatusHover,
        t("El estado de la emisión LiveLAN según lo informa el servidor"),
    ),
    (
        Msg::LiveLanUnavailableDetail,
        t(
            "Este servidor no tiene escucha LiveLAN, así que nadie puede mirar todavía. El servidor que Studio inicia en este equipo funciona sin medios nativos y no puede emitir LiveLAN: conecte Studio a un servidor iniciado con freemixd serve <show.freemix> --native-media --live-lan-listen 0.0.0.0:8090",
        ),
    ),
    (
        Msg::LiveLanFailedDetail,
        t("La emisión no pudo iniciarse; el servidor lo reintenta cada pocos segundos"),
    ),
    (Msg::LiveLanViewers, t("{count} de {max} mirando")),
    (Msg::LiveLanEgress, t("{kbps} kbps hacia los espectadores")),
    (Msg::LiveLanEnable, t("Compartir en la red local")),
    (
        Msg::LiveLanEnableHover,
        t("Inicia o detiene la emisión LiveLAN; al detenerla termina la sesión de cada espectador"),
    ),
    (Msg::LiveLanSource, t("Los espectadores ven")),
    (
        Msg::LiveLanSourceHover,
        t(
            "Elija qué ven los espectadores: Programa, o una salida con el audio que esa salida escucha",
        ),
    ),
    (
        Msg::LiveLanSourceOptionHover,
        t("Emitir {source} a los espectadores"),
    ),
    (Msg::LiveLanMaxViewers, t("Límite de espectadores")),
    (
        Msg::LiveLanMaxViewersHover,
        t(
            "Cuántas personas pueden mirar a la vez (de 1 a 32); una más espera hasta que se libere un puesto",
        ),
    ),
    (Msg::LiveLanAddress, t("Dirección para espectadores")),
    (Msg::LiveLanAddressNone, t("Aún no hay dirección")),
    (Msg::LiveLanCopy, t("Copiar dirección")),
    (
        Msg::LiveLanCopyHover,
        t("Copia la dirección para espectadores al portapapeles para compartirla"),
    ),
    (Msg::LiveLanPinLabel, t("PIN de espectadores")),
    (
        Msg::LiveLanPinHover,
        t(
            "De 4 a 12 dígitos que los espectadores escriben antes de mirar. Se guarda con el proyecto y no vuelve a mostrarse",
        ),
    ),
    (
        Msg::LiveLanPinStored,
        t("Hay un PIN; los espectadores deben escribirlo"),
    ),
    (
        Msg::LiveLanPinNone,
        t("Sin PIN: cualquiera en la red puede mirar"),
    ),
    (Msg::LiveLanPinSet, t("Establecer PIN")),
    (
        Msg::LiveLanPinSetHover,
        t(
            "Sustituye el PIN guardado por el escrito; los espectadores actuales deben introducir el nuevo",
        ),
    ),
    (Msg::LiveLanPinClear, t("Quitar PIN")),
    (
        Msg::LiveLanPinClearHover,
        t("Permite que cualquiera en la red mire sin PIN"),
    ),
    (Msg::LiveLanReasonPinDigits, t("escriba de 4 a 12 dígitos")),
    (Msg::LiveLanReasonNoPin, t("no hay ningún PIN")),
    (
        Msg::LiveLanReasonNoAddress,
        t("el servidor no ha informado ninguna dirección"),
    ),
    // Web access (RC-007, Protocol 2.71).
    (Msg::MenuWebAccess, t("Acceso web (teléfonos y tabletas)…")),
    (
        Msg::MenuWebAccessHover,
        t(
            "Empareja teléfonos y tabletas de la red local con las páginas web de tally y de operador, y revócalos",
        ),
    ),
    (Msg::WebAccessTitle, t("Acceso web")),
    (
        Msg::WebAccessIntro,
        t(
            "Los teléfonos y tabletas de esta red abren la página web de tally o de operador. Cada uno se empareja una vez con un código de un solo uso que se muestra aquí y guarda su propio token, que puede revocar en cualquier momento.",
        ),
    ),
    (Msg::WebAccessAddress, t("Dirección de la página")),
    (
        Msg::WebAccessAddressNone,
        t("Esperando la respuesta del daemon…"),
    ),
    (Msg::WebAccessCopy, t("Copiar dirección de la página")),
    (
        Msg::WebAccessCopyHover,
        t(
            "Copia la dirección de la página web al portapapeles para escribirla o enviarla a un teléfono",
        ),
    ),
    (Msg::WebAccessTls, t("Cifrado de las páginas")),
    (Msg::WebAccessTlsOn, t("TLS activado")),
    (
        Msg::WebAccessTlsOff,
        t("TLS desactivado: solo este equipo puede abrir las páginas"),
    ),
    (
        Msg::WebAccessTlsHover,
        t(
            "Indica si los teléfonos llegan a las páginas por HTTPS. Sin TLS la pasarela solo escucha en este equipo",
        ),
    ),
    (
        Msg::WebAccessFingerprint,
        t("Huella del certificado (SHA-256)"),
    ),
    (
        Msg::WebAccessFingerprintHover,
        t(
            "Compárela con los detalles del certificado que muestra el navegador del teléfono antes de confiar en él",
        ),
    ),
    (Msg::WebAccessCopyFingerprint, t("Copiar huella")),
    (Msg::WebAccessTrust, t("Confianza")),
    (
        Msg::WebAccessTrustLocalCa,
        t("Autoridad de certificación de FreeMix"),
    ),
    (Msg::WebAccessTrustOperator, t("Su propio certificado")),
    (
        Msg::WebAccessTrustPlain,
        t("Ninguna: HTTP sin cifrar solo en este equipo"),
    ),
    (
        Msg::WebAccessTrustHover,
        t(
            "Cómo llegan los teléfonos a confiar en las páginas: instale una vez el certificado de FreeMix de este equipo en cada teléfono, o sirva su propio certificado (--web-tls-cert); la huella sigue siendo la comprobación de reserva",
        ),
    ),
    (
        Msg::WebAccessValidUntil,
        t("Certificado válido hasta el {date}"),
    ),
    (
        Msg::WebAccessValidUntilHover,
        t(
            "La fecha de caducidad del certificado servido; la autoridad de FreeMix lo renueva por sí sola",
        ),
    ),
    (
        Msg::WebAccessProblemReloadFailed,
        t(
            "Sus nuevos archivos de certificado no superaron la validación; se sigue sirviendo el certificado anterior (vea el registro del daemon)",
        ),
    ),
    (
        Msg::WebAccessProblemHostMismatch,
        t(
            "El certificado no nombra la dirección de la página, así que los teléfonos mostrarán una advertencia",
        ),
    ),
    (
        Msg::WebAccessProblemExpiring,
        t("El certificado caduca en menos de 30 días; sustitúyalo"),
    ),
    (
        Msg::WebAccessProblemHover,
        t("Un problema del certificado servido que los teléfonos notarán"),
    ),
    (Msg::WebAccessReach, t("Abrir en un teléfono")),
    (
        Msg::WebAccessReachHover,
        t(
            "Todas las direcciones que llegan a las páginas desde esta red; elija una para mostrarla como código QR",
        ),
    ),
    (
        Msg::WebAccessReachOptionHover,
        t("Mostrar {url} como código QR"),
    ),
    (Msg::WebAccessMdns, t("Anunciado en la red como {name}")),
    (
        Msg::WebAccessMdnsHover,
        t(
            "Los teléfonos y equipos de esta red encuentran las páginas con este nombre (mDNS / Bonjour); algunos teléfonos Android necesitan una dirección",
        ),
    ),
    (
        Msg::WebAccessOpenQrHover,
        t("Escanee con la cámara del teléfono para abrir {url}"),
    ),
    (Msg::WebAccessCopyChosen, t("Copiar la dirección elegida")),
    (
        Msg::WebAccessCopyChosenHover,
        t("Copia {url} al portapapeles"),
    ),
    (
        Msg::WebAccessInstall,
        t("Instalar el certificado de FreeMix en los teléfonos…"),
    ),
    (
        Msg::WebAccessInstallHide,
        t("Ocultar los pasos de instalación del certificado"),
    ),
    (
        Msg::WebAccessInstallHover,
        t(
            "Muestra cómo hacer que cada teléfono confíe en las páginas de este equipo, para que se abran sin advertencia",
        ),
    ),
    (
        Msg::WebAccessInstallIntro,
        t(
            "Instale una vez el certificado de FreeMix de este equipo en cada teléfono o tableta; así sus páginas se abren sin advertencia. Escanee el código o abra la dirección en el teléfono (acepte la advertencia esa única vez) y siga los pasos para el teléfono.",
        ),
    ),
    (
        Msg::WebAccessInstallQrHover,
        t("Escanee con la cámara del teléfono para descargar el certificado de FreeMix"),
    ),
    (
        Msg::WebAccessCopyCaLink,
        t("Copiar dirección del certificado"),
    ),
    (
        Msg::WebAccessCopyCaLinkHover,
        t("Copia la dirección de descarga del certificado para enviarla a un teléfono"),
    ),
    (
        Msg::WebAccessCaFingerprint,
        t("Huella del certificado de FreeMix (SHA-256)"),
    ),
    (
        Msg::WebAccessCaFingerprintHover,
        t(
            "El teléfono muestra esta huella en los detalles del certificado; compárela antes de confiar en él",
        ),
    ),
    (
        Msg::WebAccessCopyCaFingerprint,
        t("Copiar huella del certificado"),
    ),
    (
        Msg::WebAccessCopyCaFingerprintHover,
        t("Copia la huella del certificado de FreeMix al portapapeles"),
    ),
    (
        Msg::WebAccessInstallIos,
        t(
            "iPhone y iPad: abra la descarga y luego Ajustes > Perfil descargado > Instalar; después, Ajustes > General > Información > Ajustes de confianza de certificados y active la confianza total para el certificado de FreeMix.",
        ),
    ),
    (
        Msg::WebAccessInstallAndroid,
        t(
            "Android: Ajustes > Seguridad > Cifrado y credenciales > Instalar un certificado > Certificado de CA, y elija el archivo descargado.",
        ),
    ),
    (
        Msg::WebAccessInstallComputer,
        t(
            "Equipos: abra el archivo descargado y añádalo a los certificados raíz de confianza (Acceso a Llaveros en macOS, Certificados en Windows).",
        ),
    ),
    (
        Msg::WebAccessCopyFingerprintHover,
        t("Copia la huella del certificado al portapapeles"),
    ),
    (Msg::WebAccessRole, t("Emparejar como")),
    (
        Msg::WebAccessRoleHover,
        t("Elija qué podrá hacer el siguiente dispositivo emparejado"),
    ),
    (Msg::WebAccessRoleViewer, t("Espectador (luz de tally)")),
    (Msg::WebAccessRoleAudio, t("Audio (silencia canales)")),
    (
        Msg::WebAccessRoleOperator,
        t("Operador (conmuta, graba, emite)"),
    ),
    (
        Msg::WebAccessRoleGraphics,
        t("Gráficos (edita textos de títulos en el navegador)"),
    ),
    (
        Msg::WebAccessRoleOptionHover,
        t("Emparejar el siguiente dispositivo como {role}"),
    ),
    (Msg::WebAccessStart, t("Iniciar emparejamiento")),
    (
        Msg::WebAccessStartHover,
        t(
            "Muestra un código de un solo uso y un código QR que emparejan un dispositivo en dos minutos; un código nuevo sustituye al anterior",
        ),
    ),
    (Msg::WebAccessCancel, t("Cancelar emparejamiento")),
    (
        Msg::WebAccessCancelHover,
        t("Retira el código para que nadie pueda emparejarse con él"),
    ),
    (
        Msg::WebAccessScan,
        t(
            "Escanee el código QR con la cámara del teléfono, o abra la dirección de la página y escriba este código:",
        ),
    ),
    (
        Msg::WebAccessQrHover,
        t(
            "Abre la página con un código de emparejamiento de un solo uso y la huella del certificado; el código sirve una sola vez",
        ),
    ),
    (
        Msg::WebAccessCodeHover,
        t(
            "Escriba este código en la pantalla de emparejamiento del teléfono; cinco intentos fallidos lo cancelan",
        ),
    ),
    (Msg::WebAccessPairingRole, t("Empareja como: {role}")),
    (Msg::WebAccessNoPairing, t("Ningún emparejamiento en curso")),
    (Msg::WebAccessDevices, t("Dispositivos emparejados")),
    (
        Msg::WebAccessNoDevices,
        t("Aún no hay ningún dispositivo emparejado"),
    ),
    (Msg::WebAccessColName, t("Dispositivo")),
    (Msg::WebAccessColRole, t("Acceso")),
    (Msg::WebAccessColPaired, t("Emparejado")),
    (Msg::WebAccessColSeen, t("Última conexión")),
    (Msg::WebAccessNever, t("nunca")),
    (Msg::WebAccessWhen, t("{date}, {time} UTC")),
    (Msg::WebAccessRevokeName, t("Revocar {name}")),
    (
        Msg::WebAccessRevokeHover,
        t("Desempareja {name}: su token deja de funcionar y sus páginas abiertas se desconectan"),
    ),
    (
        Msg::WebAccessRefusedPermission,
        t(
            "Gestionar dispositivos web requiere el rol de administrador (inicie Studio con --role admin)",
        ),
    ),
    (
        Msg::WebAccessRefusedWebSession,
        t(
            "Los dispositivos web no se pueden gestionar por una conexión web; conecte Studio directamente al daemon",
        ),
    ),
    (
        Msg::WebAccessRefusedUnavailable,
        t("Este daemon no sirve ninguna pasarela web (inícielo con --web-listen)"),
    ),
    (
        Msg::WebAccessRefusedUnknownDevice,
        t("Ese dispositivo ya estaba desemparejado"),
    ),
    (
        Msg::WebAccessRefusedTooMany,
        t("Todas las plazas de dispositivo están ocupadas; revoque primero un dispositivo"),
    ),
    (
        Msg::WebAccessRefusedStorage,
        t("El daemon no pudo guardar el cambio; revise su directorio de estado"),
    ),
    (
        Msg::WebAccessReasonNoAnswer,
        t("el daemon aún no ha respondido"),
    ),
    (
        Msg::WebAccessReasonNoGateway,
        t("el daemon no sirve ninguna pasarela web"),
    ),
    (
        Msg::WebAccessReasonNoPairing,
        t("no hay ningún emparejamiento en curso"),
    ),
    (
        Msg::WebAccessExpires,
        plural("Caduca en {count} segundo", "Caduca en {count} segundos"),
    ),
    (Msg::MenuUserShortcuts, t("Atajos del proyecto…")),
    (
        Msg::MenuUserShortcutsHover,
        t(
            "Abre los atajos del proyecto: teclas y direcciones OSC que ejecutan acciones de cue, una lista de cues o un guion",
        ),
    ),
    (Msg::UserShortcutsTitle, t("Atajos del proyecto")),
    (
        Msg::UserShortcutsIntro,
        t(
            "Un atajo ejecuta acciones de cue juntas en un mismo fotograma, maneja una lista de cues o ejecuta un guion, desde una tecla de esta consola o un mensaje OSC a /freemix/user/<nombre>. Los atajos se guardan con el proyecto, así que todas las consolas los comparten. Las teclas solo actúan cuando ningún diálogo ni campo de texto tiene el teclado.",
        ),
    ),
    (Msg::UserShortcutsEmpty, t("Aún no hay atajos")),
    (
        Msg::UserShortcutsEmptyDetail,
        t(
            "Pulse Nuevo atajo para dar una tecla o un nombre OSC a acciones de cue, a una lista de cues o a un guion.",
        ),
    ),
    (
        Msg::UserShortcutsGroup,
        t("Atajos del proyecto (menú Editar)"),
    ),
    (
        Msg::UserShortcutsReferenceEmpty,
        t(
            "Este proyecto no tiene atajos con tecla ni nombre OSC; añádalos en Editar, Atajos del proyecto…",
        ),
    ),
    (Msg::UserShortcutListOff, t("{name} (desactivado)")),
    (Msg::UserShortcutListHover, t("Editar el atajo {name}")),
    (Msg::UserShortcutNew, t("Nuevo atajo")),
    (
        Msg::UserShortcutNewHover,
        t("Empieza un atajo nuevo; se une al proyecto al guardarlo"),
    ),
    (Msg::UserShortcutNameLabel, t("Nombre del atajo")),
    (
        Msg::UserShortcutNameHover,
        t(
            "El nombre que muestran esta lista, la referencia de teclado y los rechazos (de 1 a 64 bytes)",
        ),
    ),
    (Msg::UserShortcutEnabled, t("Activado")),
    (
        Msg::UserShortcutEnabledHover,
        t(
            "Un atajo desactivado conserva su tecla y su nombre OSC, pero no hace nada al pulsarlo o enviarlo",
        ),
    ),
    (Msg::UserShortcutKeyLabel, t("Tecla del atajo")),
    (Msg::UserShortcutKeyNone, t("Sin tecla")),
    (
        Msg::UserShortcutKeyCapturing,
        t("Pulse la tecla o el botón del ratón ahora (Esc conserva la anterior)"),
    ),
    (
        Msg::UserShortcutKeyHover,
        t(
            "La tecla o el botón del ratón (central, atrás o adelante) que dispara este atajo en esta consola, con Ctrl (Cmd en macOS), Mayús y Alt tal como se pulsaron",
        ),
    ),
    (Msg::UserShortcutSetKey, t("Asignar tecla")),
    (
        Msg::UserShortcutSetKeyHover,
        t(
            "Captura la siguiente tecla o botón del ratón (central, atrás o adelante) que pulse, con sus modificadores, como tecla de este atajo",
        ),
    ),
    (Msg::UserShortcutClearKey, t("Quitar tecla")),
    (
        Msg::UserShortcutClearKeyHover,
        t("Quita la tecla; el atajo sigue respondiendo a su nombre OSC"),
    ),
    (
        Msg::UserShortcutKeyReserved,
        t("La consola ya usa {keys} para una de sus órdenes; elija otra tecla"),
    ),
    (
        Msg::UserShortcutKeyTaken,
        t("El atajo {name} ya usa esta tecla"),
    ),
    (
        Msg::UserShortcutKeyUnsupported,
        t(
            "Esa tecla no puede ser tecla de atajo (Control en macOS, una tecla del portapapeles o un botón del ratón como segunda tecla de un acorde); pulse otra",
        ),
    ),
    (Msg::UserShortcutOscLabel, t("Nombre OSC")),
    (
        Msg::UserShortcutOscHover,
        t(
            "Un mensaje OSC sin argumentos a /freemix/user/ y este nombre dispara el atajo mediante la escucha OSC de Studio (minúsculas, dígitos, - y _; hasta 32)",
        ),
    ),
    (
        Msg::UserShortcutOscAddressHover,
        t("La dirección OSC que dispara este atajo"),
    ),
    (
        Msg::UserShortcutOscTaken,
        t("El atajo {name} ya responde a este nombre OSC"),
    ),
    (Msg::UserShortcutOscOnly, t("Solo OSC")),
    (Msg::UserShortcutOscHint, t("Ninguno (opcional)")),
    (Msg::UserShortcutActionLabel, t("Ejecuta")),
    (
        Msg::UserShortcutKindActions,
        t("Acciones de cue en un fotograma"),
    ),
    (
        Msg::UserShortcutKindCue,
        t("Una operación de lista de cues"),
    ),
    (Msg::UserShortcutKindScript, t("Un guion del proyecto")),
    (
        Msg::UserShortcutKindHover,
        t(
            "Elija qué ejecuta el atajo: acciones de cue juntas, el GO, Detener, Atrás, Saltar o Reiniciar de una lista de cues, o un guion",
        ),
    ),
    (
        Msg::UserShortcutActionsNote,
        t(
            "Hasta 16 acciones se ejecutan juntas en un fotograma, o ninguna. Un atajo no puede esperar: ponga los pasos temporizados en un cue y haga que el atajo le dé GO.",
        ),
    ),
    (Msg::UserShortcutNoneChosen, t("(ninguno elegido)")),
    (
        Msg::UserShortcutListPickHover,
        t("La lista de cues que maneja el atajo"),
    ),
    (
        Msg::UserShortcutOperationHover,
        t("Qué hace el atajo con la lista de cues"),
    ),
    (
        Msg::UserShortcutCuePickHover,
        t("El cue que dispara el atajo, esté donde esté la espera de la lista"),
    ),
    (
        Msg::UserShortcutScriptPickHover,
        t("El guion del proyecto que ejecuta el atajo"),
    ),
    (Msg::UserShortcutOpGo, t("GO del cue en espera")),
    (Msg::UserShortcutOpGoCue, t("GO de un cue elegido")),
    (Msg::UserShortcutOpStop, t("Detener la lista")),
    (Msg::UserShortcutOpBack, t("Espera un cue atrás")),
    (Msg::UserShortcutOpSkip, t("Espera un cue adelante")),
    (Msg::UserShortcutOpReset, t("Reiniciar la lista")),
    (Msg::UserShortcutSave, t("Guardar atajo")),
    (
        Msg::UserShortcutSaveHover,
        t("Guarda el atajo en el proyecto como una edición que se puede deshacer"),
    ),
    (Msg::UserShortcutRevert, t("Descartar cambios del atajo")),
    (
        Msg::UserShortcutRevertHover,
        t("Vuelve al atajo tal como está guardado en el proyecto"),
    ),
    (Msg::UserShortcutFire, t("Disparar ahora")),
    (
        Msg::UserShortcutFireHover,
        t("Ejecuta ahora en el servidor el atajo guardado, como lo haría su tecla"),
    ),
    (Msg::UserShortcutRemove, t("Eliminar atajo")),
    (
        Msg::UserShortcutRemoveHover,
        t("Quita el atajo del proyecto como una edición que se puede deshacer"),
    ),
    (
        Msg::UserShortcutLastFireHover,
        t(
            "La última vez que se ejecutaron las acciones de cue de este atajo, desde cualquier consola",
        ),
    ),
    (
        Msg::UserShortcutNotFired,
        t("El atajo {name} no se disparó: {reason}"),
    ),
    (
        Msg::UserShortcutReasonDisabled,
        t("el atajo está desactivado"),
    ),
    (
        Msg::UserShortcutReasonNoList,
        t("la lista de cues del atajo no está en el proyecto"),
    ),
    (
        Msg::UserShortcutReasonNoScript,
        t("el guion del atajo no está en el proyecto"),
    ),
    (
        Msg::UserShortcutReasonNoActions,
        t("añada al menos una acción"),
    ),
    (
        Msg::UserShortcutReasonTooManyActions,
        t("un atajo ejecuta como mucho 16 acciones"),
    ),
    (
        Msg::UserShortcutReasonWaits,
        t("un atajo no puede esperar; ponga los pasos temporizados en un cue"),
    ),
    (
        Msg::UserShortcutReasonUnchanged,
        t("el atajo no ha cambiado"),
    ),
    (
        Msg::UserShortcutReasonReserved,
        t("la tecla es una de las teclas propias de la consola"),
    ),
    (
        Msg::UserShortcutReasonKeyTaken,
        t("otro atajo usa esta tecla"),
    ),
    (
        Msg::UserShortcutReasonOsc,
        t("un nombre OSC tiene de 1 a 32 minúsculas, dígitos, - o _"),
    ),
    (
        Msg::UserShortcutReasonOscTaken,
        t("otro atajo responde a este nombre OSC"),
    ),
    (
        Msg::UserShortcutReasonInvalid,
        t("las acciones toman Programa dos veces o llevan más de 512 bytes de texto"),
    ),
    (
        Msg::UserShortcutReasonLimit,
        t("el proyecto ya tiene 64 atajos"),
    ),
    (
        Msg::UserShortcutReasonCapturing,
        t("pulse primero la tecla que quiere asignar"),
    ),
    (Msg::UserShortcutReasonNoKey, t("el atajo no tiene tecla")),
    (
        Msg::UserShortcutReasonNotSaved,
        t("el atajo aún no está en el proyecto"),
    ),
    (Msg::UserShortcutReasonUnsaved, t("guarde primero el atajo")),
    (
        Msg::UserShortcutReasonNoLists,
        t("el proyecto no tiene listas de cues"),
    ),
    (
        Msg::UserShortcutReasonNoScripts,
        t("el proyecto no tiene guiones"),
    ),
    (Msg::UserShortcutReasonNoCues, t("la lista no tiene cues")),
    (
        Msg::UserShortcutLastFire,
        plural(
            "Último disparo en el fotograma {frame}: {count} acción",
            "Último disparo en el fotograma {frame}: {count} acciones",
        ),
    ),
    (Msg::UserShortcutFile, t("Archivo de atajos")),
    (
        Msg::UserShortcutFileHover,
        t(
            "Compartir los atajos del proyecto con otro proyecto o consola mediante un archivo .fmshortcuts",
        ),
    ),
    (
        Msg::UserShortcutPathHint,
        t("Ruta a un archivo .fmshortcuts"),
    ),
    (
        Msg::UserShortcutPathHover,
        t(
            "La ruta de un archivo .fmshortcuts en este equipo, para exportar los atajos o importar un conjunto",
        ),
    ),
    (Msg::UserShortcutExport, t("Exportar conjunto")),
    (
        Msg::UserShortcutExportHover,
        t(
            "Escribir todos los atajos del proyecto, tal como están guardados, en el archivo .fmshortcuts de la ruta",
        ),
    ),
    (Msg::UserShortcutImport, t("Importar conjunto")),
    (
        Msg::UserShortcutImportHover,
        t(
            "Leer el archivo .fmshortcuts de la ruta en el proyecto: un atajo con un nombre que el proyecto ya usa lo reemplaza, los demás se añaden, y no se importa nada si una tecla o un nombre OSC chocaran",
        ),
    ),
    (
        Msg::UserShortcutExported,
        plural(
            "{count} atajo exportado a {file}",
            "{count} atajos exportados a {file}",
        ),
    ),
    (
        Msg::UserShortcutImporting,
        t(
            "Importando atajos desde {file}: {added} nuevos, {replaced} reemplazados, {unchanged} sin cambios",
        ),
    ),
    (
        Msg::UserShortcutFileFailed,
        t("Archivo de atajos {file}: {error}"),
    ),
    (
        Msg::UserShortcutImportReserved,
        t("el atajo {name} usa {keys}, una de las teclas propias de la consola"),
    ),
    (
        Msg::UserShortcutReasonNoShortcuts,
        t("el proyecto no tiene atajos"),
    ),
    (
        Msg::UserShortcutImportTooMany,
        t("el proyecto tendría {count} atajos; caben 64 como máximo"),
    ),
    (
        Msg::UserShortcutImportKeyTaken,
        t("el atajo importado {imported} usa la tecla de {existing}"),
    ),
    (
        Msg::UserShortcutImportOscTaken,
        t("el atajo importado {imported} usa el nombre OSC de {existing}"),
    ),
    (
        Msg::UserShortcutImportSwap,
        t(
            "{first} y {second} intercambiarían una tecla o un nombre OSC; borre uno de ellos primero",
        ),
    ),
    (
        Msg::UserShortcutImportNoId,
        t("no queda ningún id de atajo libre"),
    ),
    (Msg::UserShortcutLocalLabel, t("Esta consola")),
    (
        Msg::UserShortcutLocalHover,
        t(
            "La tecla que dispara este atajo en esta consola. Se guarda en las preferencias de esta consola, no en el proyecto, así que las demás consolas conservan la tecla del proyecto",
        ),
    ),
    (Msg::UserShortcutLocalProject, t("Tecla del proyecto")),
    (Msg::UserShortcutLocalNone, t("Sin tecla en esta consola")),
    (
        Msg::UserShortcutLocalCapturing,
        t("Pulse ahora la tecla o el botón del ratón de esta consola (Esc conserva la anterior)"),
    ),
    (Msg::UserShortcutSetLocal, t("Tecla de la consola")),
    (
        Msg::UserShortcutSetLocalHover,
        t(
            "Capturar la próxima tecla o botón del ratón (central, atrás o adelante) que pulse como tecla de este atajo solo en esta consola; el proyecto y las demás consolas conservan la suya",
        ),
    ),
    (Msg::UserShortcutUseProject, t("Usar la tecla del proyecto")),
    (
        Msg::UserShortcutUseProjectHover,
        t(
            "Quitar la tecla propia de esta consola para que el atajo vuelva a responder aquí a la tecla del proyecto",
        ),
    ),
    (Msg::UserShortcutNoLocalKey, t("Sin tecla aquí")),
    (
        Msg::UserShortcutNoLocalKeyHover,
        t("Dejar el atajo sin tecla en esta consola; las demás consolas conservan la suya"),
    ),
    (
        Msg::UserShortcutLocalTaken,
        t("En esta consola el atajo {name} ya responde a esta tecla"),
    ),
    (
        Msg::UserShortcutLocalShadowed,
        t("En esta consola {name} responde a esta tecla del proyecto en su lugar"),
    ),
    (Msg::UserShortcutListLocal, t("{keys} (esta consola)")),
    (
        Msg::UserShortcutReasonProjectKeyHere,
        t("el atajo ya responde aquí a la tecla del proyecto"),
    ),
    (
        Msg::UserShortcutReasonNoKeyHere,
        t("el atajo ya no tiene tecla en esta consola"),
    ),
    (
        Msg::UserShortcutReasonOverrideLimit,
        t("esta consola no guarda más teclas propias"),
    ),
    (Msg::UserShortcutSetThen, t("Añadir segunda tecla")),
    (
        Msg::UserShortcutSetThenHover,
        t(
            "Convertir la tecla en un acorde: capturar una segunda tecla pulsada después de esta (Ctrl+K y luego 1). La segunda puede ser cualquier tecla, incluso una que la consola usa sola",
        ),
    ),
    (Msg::UserShortcutClearThen, t("Quitar segunda tecla")),
    (
        Msg::UserShortcutClearThenHover,
        t("Volver a una sola tecla, sin la segunda"),
    ),
    (
        Msg::UserShortcutThenCapturing,
        t("Pulse ahora la segunda tecla (Esc conserva la anterior)"),
    ),
    (Msg::UserShortcutScopeLabel, t("Responde en")),
    (
        Msg::UserShortcutScopeHover,
        t(
            "Dónde dispara la tecla el atajo: en toda la consola, solo mientras un panel tiene el foco (el último panel en que hizo clic) o solo mientras la ventana propia de un panel desacoplado tiene el foco. Paneles o ventanas distintos pueden usar la misma tecla",
        ),
    ),
    (Msg::UserShortcutScopeAnywhere, t("En toda la consola")),
    (Msg::UserShortcutInPanel, t("{keys} en {panel}")),
    (
        Msg::UserShortcutChordPending,
        t("{keys} pulsada: pulse la segunda tecla"),
    ),
    (
        Msg::UserShortcutReasonNoFirstKey,
        t("el atajo aún no tiene tecla"),
    ),
    (
        Msg::UserShortcutReasonNoThen,
        t("la tecla no tiene segunda tecla"),
    ),
    (
        Msg::UserShortcutScopeWindow,
        t("Ventana de {panel} (desacoplada)"),
    ),
    (
        Msg::UserShortcutScopeWindowHover,
        t(
            "Disparar solo mientras este panel está desacoplado en su propia ventana y esa ventana tiene el foco; la ventana principal de la consola nunca responde a la tecla",
        ),
    ),
    (
        Msg::UserShortcutInWindow,
        t("{keys} en la ventana de {panel}"),
    ),
    (Msg::CueActSlideNext, t("Diapositiva siguiente")),
    (Msg::CueActSlidePrevious, t("Diapositiva anterior")),
    (Msg::CueActSlideFirst, t("Primera diapositiva")),
    (Msg::CueActSlideLast, t("Última diapositiva")),
    (Msg::CueActSlideGoto, t("Ir a la diapositiva")),
    (
        Msg::CueParamSlideshowHover,
        t("La entrada de presentación o documento cuyas diapositivas o páginas avanza esta acción"),
    ),
    (
        Msg::CueParamSlideHover,
        t(
            "Número de diapositiva o página que mostrar, contando desde 1; pasado el final muestra la última",
        ),
    ),
    (
        Msg::CuePvSlideNext,
        t("Mostrar la diapositiva siguiente de {input}"),
    ),
    (
        Msg::CuePvSlidePrevious,
        t("Mostrar la diapositiva anterior de {input}"),
    ),
    (
        Msg::CuePvSlideFirst,
        t("Mostrar la primera diapositiva de {input}"),
    ),
    (
        Msg::CuePvSlideLast,
        t("Mostrar la última diapositiva de {input}"),
    ),
    (
        Msg::CuePvSlideGoto,
        t("Mostrar la diapositiva {page} de {input}"),
    ),
    (
        Msg::CuePvRefNotSlideshow,
        t("no es una presentación ni un documento"),
    ),
    (Msg::NameImage, t("Imagen")),
    (Msg::KindImageKey, t("IMAGEN FIJA")),
    (
        Msg::KindImageHover,
        t(
            "Una imagen fija (PNG, JPEG, WebP, GIF, BMP, TIFF o SVG) colocada en el cuadro del show; el servidor la copia al proyecto",
        ),
    ),
    (Msg::NameSlideshow, t("Presentación")),
    (Msg::KindSlideshowKey, t("PRESENTACIÓN")),
    (
        Msg::KindSlideshowHover,
        t(
            "Imágenes de una carpeta o de una lista mostradas una tras otra, con corte o fundido, Ken Burns opcional y control de siguiente/anterior",
        ),
    ),
    (Msg::StPicture, t("Archivo de imagen")),
    (
        Msg::StPictureHover,
        t(
            "La imagen que mostrar: una ruta absoluta en la máquina del servidor (se copia al proyecto) o una URI asset:// que ya esté en él; soltar un archivo en la ventana la rellena",
        ),
    ),
    (
        Msg::StPathCaption,
        t("Ruta absoluta en la máquina del servidor, o asset://"),
    ),
    (Msg::StFit, t("Colocación")),
    (Msg::StFitFit, t("AJUSTAR")),
    (
        Msg::StFitFitHover,
        t(
            "Mostrar la imagen entera lo más grande posible, conservando su forma; el resto del cuadro queda vacío",
        ),
    ),
    (Msg::StFitFill, t("LLENAR")),
    (
        Msg::StFitFillHover,
        t("Cubrir todo el cuadro conservando la forma de la imagen; lo que sobra se recorta"),
    ),
    (Msg::StFitStretch, t("ESTIRAR")),
    (
        Msg::StFitStretchHover,
        t("Estirar la imagen exactamente al cuadro, cambiando su forma si hace falta"),
    ),
    (Msg::StFitOneToOne, t("1:1")),
    (
        Msg::StFitOneToOneHover,
        t(
            "Un píxel de imagen por píxel del cuadro, centrado: una imagen pequeña conserva su tamaño y una grande se recorta",
        ),
    ),
    (Msg::StSource, t("Diapositivas de")),
    (Msg::StSourceFolder, t("CARPETA")),
    (
        Msg::StSourceFolderHover,
        t(
            "Todas las imágenes de una carpeta, en orden de nombre; el servidor copia la carpeta al proyecto y sigue esa copia, así que las imágenes que se añadan o quiten entran o salen del show",
        ),
    ),
    (Msg::StSourceList, t("LISTA")),
    (
        Msg::StSourceListHover,
        t("Una lista explícita de hasta 64 imágenes, en tu orden, cada una con un pie opcional"),
    ),
    (Msg::StFolder, t("Carpeta de imágenes")),
    (
        Msg::StFolderHover,
        t(
            "La carpeta de imágenes: una ruta absoluta en la máquina del servidor (se copia al proyecto) o una carpeta asset:// que ya esté en él; soltar una carpeta en la ventana la rellena",
        ),
    ),
    (Msg::StPictures, t("Lista de imágenes")),
    (
        Msg::StPicturesHover,
        t(
            "Una imagen por línea, como ruta o URI asset://; escribe ' | ' y un texto tras la ruta para ponerle pie; los archivos soltados se añaden",
        ),
    ),
    (Msg::StPicturesCaption, t("Una por línea; ruta | pie")),
    (Msg::StTiming, t("Tiempos")),
    (Msg::StSlideSeconds, t("Cada diapositiva")),
    (
        Msg::StSlideSecondsHover,
        t("Cuánto dura cada diapositiva antes de la siguiente, en segundos"),
    ),
    (Msg::StCut, t("CORTE AL SIGUIENTE")),
    (
        Msg::StCutHover,
        t("Cada diapositiva sustituye a la anterior al instante"),
    ),
    (Msg::StFade, t("FUNDIDO AL SIGUIENTE")),
    (
        Msg::StFadeHover,
        t("Cada diapositiva se funde sobre la anterior"),
    ),
    (
        Msg::StFadeSecondsHover,
        t("Cuánto dura el fundido entre diapositivas, en segundos"),
    ),
    (Msg::StKenBurns, t("Ken Burns")),
    (
        Msg::StKenBurnsHover,
        t("Un paneo y zoom lentos sobre cada diapositiva mientras se muestra"),
    ),
    (Msg::StRandom, t("Orden aleatorio")),
    (
        Msg::StRandomHover,
        t("Mostrar las diapositivas en orden mezclado, el mismo cada vez que se reproduce el show"),
    ),
    (Msg::StLoop, t("Volver a empezar al final")),
    (
        Msg::StLoopHover,
        t(
            "Tras la última diapositiva, empezar otra vez por la primera; desactivado, la última se queda",
        ),
    ),
    (Msg::StAutoAdvance, t("Avanzar automáticamente")),
    (
        Msg::StAutoAdvanceHover,
        t(
            "Pasar a la siguiente diapositiva al acabar su tiempo; desactivado, cada diapositiva se queda hasta que avances",
        ),
    ),
    (Msg::StReasonPictureEmpty, t("elige un archivo de imagen")),
    (
        Msg::StReasonPictureType,
        t("no es un archivo de imagen (PNG, JPEG, WebP, GIF, BMP, TIFF o SVG)"),
    ),
    (
        Msg::StReasonPathInvalid,
        t("usa una ruta absoluta en la máquina del servidor o una URI asset://"),
    ),
    (Msg::StReasonFolderEmpty, t("elige una carpeta de imágenes")),
    (Msg::StReasonPicturesEmpty, t("añade al menos una imagen")),
    (
        Msg::StReasonPicturesTooMany,
        t("una lista admite como mucho 64 imágenes; usa una carpeta para más"),
    ),
    (
        Msg::StReasonNoSlides,
        t("la presentación no tiene diapositivas"),
    ),
    (Msg::MediaSlideshows, t("Presentaciones")),
    (
        Msg::StPageHover,
        t("La diapositiva al aire y cuántas tiene la presentación"),
    ),
    (Msg::StFirst, t("PRIMERA")),
    (Msg::StFirstHover, t("Mostrar la primera diapositiva")),
    (
        Msg::StPreviousHover,
        t("Mostrar la diapositiva anterior (la última si se repite)"),
    ),
    (
        Msg::StNextHover,
        t("Mostrar la diapositiva siguiente (la primera si se repite)"),
    ),
    (Msg::StLast, t("ÚLTIMA")),
    (Msg::StLastHover, t("Mostrar la última diapositiva")),
    (Msg::StNoSlide, t("Sin diapositiva")),
    (
        Msg::StSlideNameHover,
        t("El pie o el nombre de archivo de la diapositiva al aire"),
    ),
    (
        Msg::StSlideKeyHover,
        t("Mostrar la diapositiva {page}: {name}"),
    ),
    (Msg::ThumbSlideshowEmpty, t("Aún sin imágenes")),
    (Msg::ThumbMediaListEmpty, t("Aún sin archivos")),
    // Strip and Master FX (AU-005).
    (Msg::FxKey, t("FX")),
    (
        Msg::FxKeyHover,
        t(
            "Abre la cadena FX de este canal: polaridad, paso alto, EQ, puerta, expansor, compresor y limitador; se ilumina mientras alguna etapa está activa",
        ),
    ),
    (
        Msg::FxMasterKeyHover,
        t(
            "Abre la cadena FX del Máster, que procesa el bus de Programa sumado; se ilumina mientras alguna etapa está activa",
        ),
    ),
    (Msg::FxWindowTitle, t("FX: {strip}")),
    (Msg::FxMasterName, t("Bus máster")),
    (Msg::FxBypass, t("BYPASS")),
    (
        Msg::FxBypassHover,
        t("Puentea toda la cadena: el canal se oye sin procesar y se conservan todos los ajustes"),
    ),
    (
        Msg::FxPresetHover,
        t("Carga una cadena de partida; todas las etapas siguen siendo editables"),
    ),
    (Msg::FxPresetLabel, t("Preajuste")),
    (Msg::FxPresetFlat, t("Plano")),
    (Msg::FxPresetVoice, t("Voz")),
    (Msg::FxPresetPodcast, t("Pódcast")),
    (Msg::FxPresetMusic, t("Música")),
    (Msg::FxPresetMaster, t("Pegamento de máster")),
    (
        Msg::FxPresetOptionHover,
        t("Carga la cadena {preset} en este canal"),
    ),
    (Msg::FxCopy, t("Copiar cadena")),
    (
        Msg::FxCopyHover,
        t("Copia esta cadena para pegarla en otro canal"),
    ),
    (Msg::FxPaste, t("Pegar cadena")),
    (Msg::FxPasteHover, t("Sustituye esta cadena por la copiada")),
    (Msg::FxReset, t("Restablecer cadena")),
    (
        Msg::FxResetHover,
        t("Desactiva todas las etapas y restaura los ajustes predeterminados"),
    ),
    (
        Msg::FxReasonNothingCopied,
        t("aún no se ha copiado ninguna cadena"),
    ),
    (Msg::FxStageHighPass, t("Paso alto")),
    (Msg::FxStageEq, t("EQ")),
    (Msg::FxStageGate, t("Puerta")),
    (Msg::FxStageCompressor, t("Compresor")),
    (Msg::FxStageLimiter, t("Limitador")),
    (
        Msg::FxStageOnHover,
        t("{stage} activo: haga clic para desactivarlo"),
    ),
    (
        Msg::FxStageOffHover,
        t("{stage} inactivo: haga clic para activarlo"),
    ),
    (
        Msg::FxCurveHover,
        t(
            "Arrastre un punto numerado para ajustar la frecuencia y la ganancia de su banda, y use la rueda sobre él para cambiar su anchura (Q); arrastre HP para mover el corte del paso alto",
        ),
    ),
    (
        Msg::FxBandHover,
        t("Activa o desactiva la banda {band} del EQ"),
    ),
    (
        Msg::FxBandKindHover,
        t("La forma de la banda {band}: estante grave, pico o estante agudo"),
    ),
    (Msg::FxKindLowShelf, t("Estante grave")),
    (Msg::FxKindPeak, t("Pico")),
    (Msg::FxKindHighShelf, t("Estante agudo")),
    (Msg::FxKindOptionHover, t("Convierte esta banda en {kind}")),
    (
        Msg::FxFrequencyHover,
        t("Frecuencia de la banda en hercios"),
    ),
    (
        Msg::FxBandGainHover,
        t("Realce o atenuación de la banda en dB"),
    ),
    (
        Msg::FxQHover,
        t("Anchura de la banda: una Q mayor es más estrecha"),
    ),
    (
        Msg::FxHighPassHover,
        t("Corte del paso alto: las frecuencias más graves se atenúan 12 dB por octava"),
    ),
    (
        Msg::FxGateThresholdHover,
        t("La puerta se abre mientras el nivel supera este valor"),
    ),
    (
        Msg::FxGateRangeHover,
        t("Cuánto baja el canal la puerta cerrada"),
    ),
    (
        Msg::FxAttackHover,
        t("Con qué rapidez reacciona la etapa a una subida de nivel"),
    ),
    (
        Msg::FxHoldHover,
        t("Cuánto tiempo sigue abierta la puerta después de que baja el nivel"),
    ),
    (
        Msg::FxReleaseHover,
        t("Con qué rapidez se recupera la etapa cuando baja el nivel"),
    ),
    (
        Msg::FxCompThresholdHover,
        t("El compresor baja los niveles que superan este valor"),
    ),
    (
        Msg::FxRatioHover,
        t("Con qué intensidad se reducen los niveles por encima del umbral"),
    ),
    (
        Msg::FxKneeHover,
        t("Suaviza la entrada de la compresión alrededor del umbral"),
    ),
    (
        Msg::FxMakeupHover,
        t("Ganancia añadida tras la compresión para recuperar el nivel"),
    ),
    (
        Msg::FxCeilingHover,
        t("El limitador mantiene los picos por debajo de este nivel"),
    ),
    (Msg::FxAbbrThreshold, t("UMB")),
    (Msg::FxAbbrRange, t("RANGO")),
    (Msg::FxAbbrAttack, t("ATQ")),
    (Msg::FxAbbrHold, t("MANT")),
    (Msg::FxAbbrRelease, t("LIB")),
    (Msg::FxAbbrRatio, t("PROP")),
    (Msg::FxAbbrKnee, t("CODO")),
    (Msg::FxAbbrMakeup, t("COMP")),
    (Msg::FxAbbrCeiling, t("TECHO")),
    (Msg::FxAbbrGainReduction, t("RG")),
    (
        Msg::FxGrHover,
        t("Reducción de ganancia que aplica ahora {stage}: {db}"),
    ),
    (
        Msg::FxGrIdleHover,
        t("No se informa reducción de ganancia para {stage}"),
    ),
    (Msg::FxPendingHover, t("Enviando esta cadena al servidor")),
    (
        Msg::FxCloseHover,
        t("Cierra la ventana FX; la cadena se queda en el canal"),
    ),
    // Entradas de captura de pantalla y de ventana (IN-011).
    (Msg::NameScreenCapture, t("Captura de pantalla")),
    (Msg::KindScreenCaptureKey, t("VENTANA / PANTALLA")),
    (
        Msg::KindScreenCaptureHover,
        t(
            "Muestra una pantalla del escritorio del servidor o una ventana de una aplicación, capturadas en el servidor; la ventana se sigue por su aplicación y su título, así que se vuelve a encontrar cuando se cierra y se reabre",
        ),
    ),
    (Msg::ScTarget, t("Qué capturar")),
    (Msg::ScRefresh, t("Actualizar destinos")),
    (
        Msg::ScRefreshHover,
        t(
            "Pide al servidor que vuelva a listar sus pantallas y ventanas abiertas, por ejemplo tras abrir la ventana que quiere capturar",
        ),
    ),
    (
        Msg::ScListUnknown,
        t("Preguntando al servidor qué puede capturar…"),
    ),
    (
        Msg::ScListUnsupported,
        t("Este servidor no puede capturar su escritorio: {reason}"),
    ),
    (
        Msg::ScListError,
        t("El servidor no pudo listar su escritorio: {error}"),
    ),
    (
        Msg::ScListScreens,
        plural("{count} pantalla", "{count} pantallas"),
    ),
    (
        Msg::ScListWindows,
        plural("{count} ventana", "{count} ventanas"),
    ),
    (
        Msg::ScListCount,
        t("El servidor lista {screens} y {windows} ({backend})"),
    ),
    (Msg::ScGroupScreens, t("Pantallas")),
    (Msg::ScGroupWindows, t("Ventanas de aplicaciones")),
    (
        Msg::ScNoWindows,
        t("No hay ventanas de aplicaciones abiertas en el servidor"),
    ),
    (
        Msg::ScScreenOption,
        t("Pantalla {number} · {name} · {size}"),
    ),
    (Msg::ScScreenPrimary, t("pantalla principal")),
    (
        Msg::ScScreenOptionHover,
        t("Captura toda la pantalla {number} ({size}, en {x}, {y} del escritorio)"),
    ),
    (Msg::ScWindowOption, t("{app} · {title}")),
    (Msg::ScWindowUntitled, t("{app} (sin título)")),
    (
        Msg::ScWindowOptionHover,
        t(
            "Captura la ventana de {app} titulada \"{title}\" ({size}); se sigue por su aplicación y su título",
        ),
    ),
    (
        Msg::ScChosenMissing,
        t("Ahora no está abierto en el servidor: {target}. Se captura en cuanto aparezca"),
    ),
    (Msg::ScFollow, t("Seguir la ventana por")),
    (Msg::ScMatchExact, t("Título exacto")),
    (
        Msg::ScMatchExactHover,
        t("Captura la ventana de esta aplicación cuyo título es exactamente el de abajo"),
    ),
    (Msg::ScMatchContains, t("El título contiene")),
    (
        Msg::ScMatchContainsHover,
        t(
            "Captura la primera ventana de esta aplicación cuyo título contiene el texto de abajo, sin distinguir mayúsculas; útil cuando el título cambia, como el nombre de un documento",
        ),
    ),
    (Msg::ScMatchAny, t("Cualquier ventana de la aplicación")),
    (
        Msg::ScMatchAnyHover,
        t("Captura la primera ventana que muestre esta aplicación, sea cual sea su título"),
    ),
    (Msg::ScApp, t("Aplicación: {app}")),
    (Msg::ScTitle, t("Título de la ventana")),
    (
        Msg::ScTitleHover,
        t(
            "El título de ventana que se sigue, hasta 128 bytes; al editarlo se deja de seguir la ventana elegida arriba hasta que aparezca una ventana con este título",
        ),
    ),
    (Msg::ScCrop, t("Recorte (píxeles quitados de cada borde)")),
    (Msg::ScCropLeft, t("Borde izquierdo")),
    (Msg::ScCropTop, t("Borde superior")),
    (Msg::ScCropRight, t("Borde derecho")),
    (Msg::ScCropBottom, t("Borde inferior")),
    (
        Msg::ScCropHover,
        t(
            "Píxeles que se quitan de este borde de la pantalla o ventana antes de encajarla en la imagen (0 a 16384); por ejemplo la altura de una barra de título",
        ),
    ),
    (Msg::ScCursor, t("Mostrar el puntero del ratón")),
    (
        Msg::ScCursorHover,
        t("Dibuja el puntero del ratón del servidor en la imagen capturada"),
    ),
    (Msg::ScFps, t("Límite de imágenes por segundo")),
    (
        Msg::ScFpsHover,
        t(
            "Cuántas imágenes por segundo capturar como máximo, de 1 a 60; nunca más que la cadencia del programa",
        ),
    ),
    (Msg::ScStall, t("Mientras no esté el destino")),
    (Msg::ScStallFreeze, t("Mantener la última imagen")),
    (
        Msg::ScStallFreezeHover,
        t(
            "Sigue mostrando la última imagen capturada mientras la ventana está cerrada o la pantalla desconectada",
        ),
    ),
    (Msg::ScStallBlack, t("Mostrar negro")),
    (
        Msg::ScStallBlackHover,
        t("Muestra negro mientras la ventana está cerrada o la pantalla desconectada"),
    ),
    (Msg::ScAudio, t("Audio capturado")),
    (Msg::ScAudioNone, t("Sin audio")),
    (
        Msg::ScAudioNoneHover,
        t("Captura solo la imagen; la tira del mezclador de esta entrada queda en silencio"),
    ),
    (Msg::ScAudioSystem, t("Audio del sistema")),
    (
        Msg::ScAudioSystemHover,
        t(
            "Captura todo lo que reproduce el servidor (audio de escritorio) en la tira propia de esta entrada en el mezclador, sincronizado con la imagen",
        ),
    ),
    (Msg::ScAudioApp, t("Audio de la aplicación")),
    (
        Msg::ScAudioAppHover,
        t(
            "Captura solo el sonido de la aplicación dueña de la ventana capturada en la tira propia de esta entrada en el mezclador, sincronizado con la imagen; la aplicación se sigue oyendo en el servidor",
        ),
    ),
    (
        Msg::ScAudioAppScreenHover,
        t("el audio de la aplicación necesita una ventana; elija arriba una ventana de aplicación"),
    ),
    (
        Msg::ScAudioAppPortalHover,
        t(
            "este servidor captura mediante el diálogo para compartir del escritorio Wayland, que no da audio de la aplicación; elija el audio del sistema",
        ),
    ),
    (
        Msg::ScReasonAudio,
        t(
            "el audio de la aplicación necesita una ventana; elija una ventana u otra opción de audio",
        ),
    ),
    (Msg::ScAudioLineOpening, t("{source}: iniciando")),
    (Msg::ScAudioLineLive, t("{source}: en vivo")),
    (
        Msg::ScAudioLineNoStream,
        t(
            "{source}: todavía sin flujo de audio; la tira queda en silencio hasta que la aplicación reproduzca sonido",
        ),
    ),
    (
        Msg::ScAudioLineUnsupported,
        t("{source} no admitido aquí: {reason}"),
    ),
    (Msg::ScAudioLineFailed, t("{source} falló: {reason}")),
    (Msg::ScAudioNoReason, t("sin motivo indicado")),
    (Msg::ScAudioBadgeOpening, t("AUDIO ABRIENDO")),
    (Msg::ScAudioBadgeLive, t("AUDIO EN VIVO")),
    (Msg::ScAudioBadgeNoStream, t("SIN FLUJO")),
    (Msg::ScAudioBadgeUnsupported, t("AUDIO NO ADMITIDO")),
    (Msg::ScAudioBadgeFailed, t("AUDIO FALLÓ")),
    (
        Msg::ScAudioBadgeOpeningHover,
        t(
            "La captura está abriendo su sonido; la tira del mezclador de esta entrada queda en silencio hasta que el sonido esté en vivo",
        ),
    ),
    (
        Msg::ScAudioBadgeLiveHover,
        t(
            "El sonido se captura con la imagen y alimenta la tira propia de esta entrada en el mezclador",
        ),
    ),
    (
        Msg::ScAudioBadgeNoStreamHover,
        t(
            "La aplicación todavía no reproduce sonido; su sonido se captura en cuanto empiece a reproducirlo",
        ),
    ),
    (
        Msg::ScAudioBadgeUnsupportedHover,
        t("Este servidor no puede capturar este sonido; la imagen se sigue capturando"),
    ),
    (
        Msg::ScAudioBadgeFailedHover,
        t("Falló la captura del sonido; la imagen se sigue capturando"),
    ),
    (
        Msg::ScReasonTitle,
        t("escriba el título de ventana que se sigue"),
    ),
    (
        Msg::ScReasonRule,
        t(
            "use un nombre de aplicación y un título de 128 bytes como máximo, sin saltos de línea ni espacios en los extremos",
        ),
    ),
    (
        Msg::ScReasonCrop,
        t("use bordes de recorte de 0 a 16384 píxeles"),
    ),
    (Msg::ScReasonFps, t("use de 1 a 60 imágenes por segundo")),
    (Msg::ScBadgeGone, t("SIN DESTINO")),
    (Msg::ScBadgeUnsupported, t("NO ADMITIDO")),
    (Msg::ScBadgePick, t("ELEGIR EN SERVIDOR")),
    (Msg::ScBadgeDenied, t("DENEGADO")),
    (
        Msg::ScDetailPick,
        t(
            "el diálogo para compartir espera en el escritorio del servidor: elija allí la pantalla o la ventana",
        ),
    ),
    (
        Msg::ScPortalNote,
        t(
            "Este servidor usa un escritorio Wayland: su propio diálogo para compartir elige la pantalla o la ventana en la pantalla del servidor al empezar la captura",
        ),
    ),
    (
        Msg::ScPortalLimit,
        t("El escritorio del servidor: {reason}"),
    ),
    (Msg::ScPortalScreen, t("Una pantalla")),
    (
        Msg::ScPortalScreenHover,
        t(
            "Comparte una pantalla entera; el diálogo para compartir del servidor pregunta cuál al empezar la captura, y la elección se recuerda donde el escritorio lo permite",
        ),
    ),
    (Msg::ScPortalWindow, t("Una ventana")),
    (
        Msg::ScPortalWindowHover,
        t(
            "Comparte una ventana de aplicación; el diálogo para compartir del servidor pregunta cuál al empezar la captura, y la elección se recuerda donde el escritorio lo permite",
        ),
    ),
    (
        Msg::ScTargetPickedWindow,
        t("la ventana elegida en el servidor"),
    ),
    (Msg::ScPickAgain, t("Volver a elegir destino")),
    (
        Msg::ScPickAgainHover,
        t(
            "Pide al escritorio del servidor otra vez la pantalla o la ventana de esta captura: un escritorio Wayland muestra su diálogo para compartir en la pantalla del servidor y olvida la elección recordada; los demás escritorios vuelven a capturar",
        ),
    ),
    (Msg::ScDetail, t("Captura de {target}: {state}")),
    (Msg::ScDetailSize, t("capturando {size}")),
    (Msg::ScTargetScreen, t("la pantalla {number}")),
    (Msg::ScTargetWindow, t("la ventana de {app} \"{title}\"")),
    (
        Msg::ScTargetContains,
        t("la ventana de {app} cuyo título contiene \"{title}\""),
    ),
    (Msg::ScTargetAny, t("cualquier ventana de {app}")),
    (Msg::ThumbScreenCapture, t("Capturado en el servidor")),
    (Msg::FxPolarity, t("Ø INVERTIR")),
    (
        Msg::FxPolarityOffHover,
        t(
            "Invierte la polaridad de este canal en todos sus canales de audio, por ejemplo para un micrófono cableado en contrafase",
        ),
    ),
    (
        Msg::FxPolarityOnHover,
        t("La polaridad está invertida en todos los canales de audio: haga clic para restaurarla"),
    ),
    (Msg::FxStageExpander, t("Expansor")),
    (
        Msg::LoudnessIdleHover,
        t(
            "Aún no se ha medido la sonoridad: la sonoridad momentánea necesita 400 ms de audio y la de corto plazo 3 s",
        ),
    ),
    (
        Msg::LoudnessStripHover,
        t(
            "Sonoridad de la aportación de este canal al Programa (ITU-R BS.1770 / EBU R128): corto plazo {short} LUFS (3 s), momentánea {momentary} LUFS (400 ms), integrada {integrated} LUFS, rango de sonoridad {range} LU; pico real {peak} dBTP, máximo {max} dBTP. Reinicie la medición en la ventana FX del canal",
        ),
    ),
    (
        Msg::LoudnessMasterHover,
        t(
            "Sonoridad del Programa (ITU-R BS.1770 / EBU R128): integrada {integrated} LUFS desde que empezó o se reinició la medición (objetivo de emisión -23 LUFS), rango de sonoridad {range} LU, corto plazo {short} LUFS, momentánea {momentary} LUFS; pico real {peak} dBTP, máximo {max} dBTP (manténgalo por debajo de -1 dBTP). Reinicie la medición en la ventana FX de Master",
        ),
    ),
    (Msg::FxLoudnessLabel, t("Sonoridad")),
    (
        Msg::FxLoudnessMomentaryHover,
        t("Sonoridad momentánea: los últimos 400 ms, en LUFS"),
    ),
    (
        Msg::FxLoudnessShortTermHover,
        t("Sonoridad de corto plazo: los últimos 3 segundos, en LUFS"),
    ),
    (
        Msg::FxLoudnessIntegratedHover,
        t(
            "Sonoridad integrada: el promedio con compuerta desde que empezó o se reinició por última vez la medición, en LUFS (objetivo de emisión EBU R128 -23 LUFS)",
        ),
    ),
    (
        Msg::FxTruePeakHover,
        t(
            "Pico real en dBTP: el pico entre muestras más alto ahora mismo y desde que empezó o se reinició por última vez la medición (EBU R128 admite como máximo -1 dBTP)",
        ),
    ),
    (
        Msg::FxLoudnessRangeHover,
        t(
            "Rango de sonoridad (EBU Tech 3342): cuánto varió la sonoridad de corto plazo desde que empezó o se reinició por última vez la medición, en LU; la distancia entre sus percentiles 10 y 95 tras descartar los pasajes silenciosos",
        ),
    ),
    (Msg::FxLoudnessReset, t("Reiniciar medición")),
    (
        Msg::FxLoudnessResetHover,
        t(
            "Reinicia desde ahora la sonoridad integrada, el rango de sonoridad y el pico real máximo de este bus, por ejemplo al comenzar un programa. La sonoridad momentánea y la de corto plazo siguen midiéndose; todas las consolas ven el reinicio",
        ),
    ),
    (
        Msg::FxExpThresholdHover,
        t("El expansor baja los niveles por debajo de este valor"),
    ),
    (
        Msg::FxExpRatioHover,
        t(
            "Cuánto más rápido baja el nivel por debajo del umbral: con 2:1 cada dB por debajo se convierte en 2 dB",
        ),
    ),
    (
        Msg::FxExpRangeHover,
        t("Lo máximo que el expansor baja el canal"),
    ),
    (
        Msg::FxDelayHover,
        t(
            "Tiempo de retardo en milisegundos (hasta 1 s), exacto a la muestra: retrasa el sonido, por ejemplo para alinear un micrófono con la imagen retrasada de una cámara",
        ),
    ),
];
