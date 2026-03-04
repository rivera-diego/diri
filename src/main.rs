use anyhow::Result;
use clap::{Parser, Subcommand};
use niri_ipc::{Event, Request, Response, Action, Reply};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream as TokioUnixStream;
use tokio::net::UnixListener as TokioUnixListener;
use tokio::sync::mpsc;

/// Mensajes que enviaremos por el socket IPC local
#[derive(Debug, Serialize, Deserialize)]
enum IpcMessage {
    Fetch {
        app_id: String,
        spawn_cmd: Vec<String>,
        include_current_workspace: bool,
        focus: bool,
    },
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Inicia el daemon en background
    Daemon,

    /// Comando simplificado de Diri: busca/trae o abre una app.
    /// Ya incluye --focus e --include-current-workspace por defecto.
    /// Uso: diri fetch "com.mitchellh.ghostty" ghostty new
    Fetch {
        app_id: String,
        #[arg(required = true)]
        spawn_cmd: Vec<String>,
    },

    /// Implementacion literal de Nirius (con todas sus flags manuales)
    MoveToCurrentWorkspaceOrSpawn {
        #[arg(long)]
        app_id: String,

        #[arg(long)]
        include_current_workspace: bool,

        #[arg(long, short = 'f')]
        focus: bool,

        #[arg(required = true)]
        spawn_cmd: Vec<String>,
    },
}

// Ruta donde vivirá el socket del daemon (único por usuario)
fn get_socket_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("diri-{}.sock", users::get_current_uid()));
    path
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => {
            println!("Iniciando el daemon de Niri...");
            run_daemon().await?;
        }
        Commands::Fetch { app_id, spawn_cmd } => {
            // El comando "Fetch" es la version simplificada que ya activa todo
            let msg = IpcMessage::Fetch {
                app_id,
                spawn_cmd,
                include_current_workspace: true,
                focus: true
            };
            send_to_daemon(msg).await?;
        }
        Commands::MoveToCurrentWorkspaceOrSpawn { app_id, spawn_cmd, include_current_workspace, focus } => {
            // El comando original de Nirius para uso manual con flags
            let msg = IpcMessage::Fetch { app_id, spawn_cmd, include_current_workspace, focus };
            send_to_daemon(msg).await?;
        }
    }

    Ok(())
}

// =========================================================================
// MODO CLIENTE (Cuando ejecutas un comando desde keybinds.kdl)
// =========================================================================
async fn send_to_daemon(msg: IpcMessage) -> Result<()> {
    let socket_path = get_socket_path();
    if !socket_path.exists() {
        anyhow::bail!("El daemon no está corriendo. Ejecuta `diri daemon` primero.");
    }

    let mut stream = TokioUnixStream::connect(&socket_path).await?;
    let json = serde_json::to_string(&msg)?;

    stream.write_all(json.as_bytes()).await?;
    stream.shutdown().await?;

    // Leer respuesta (opcional, para saber si funcionó)
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    if !response.is_empty() {
        println!("{}", response);
    }

    Ok(())
}

// =========================================================================
// MODO DAEMON (El proceso que corre al fondo)
// =========================================================================
async fn run_daemon() -> Result<()> {
    // 1. Conectarse a Niri y pedir streaming de eventos en un hilo separado (porque es bloqueante)
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(32);

    std::thread::spawn(move || {
        let mut socket = match niri_ipc::socket::Socket::connect() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error conectando a niri socket: {}", e);
                return;
            }
        };

        if socket.send(Request::EventStream).is_ok() {
            let mut read_event = socket.read_events();
            loop {
                match read_event() {
                    Ok(event) => {
                        if event_tx.blocking_send(event).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    println!("Conectado a Niri IPC con éxito.");

    // 2. Preparar nuestro socket local (para escuchar comandos del cliente)
    let socket_path = get_socket_path();
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?; // Limpiar socket anterior si quedó suelto
    }
    let ipc_listener = TokioUnixListener::bind(&socket_path)?;
    println!("Escuchando comandos en: {:?}", socket_path);

    let mut autofill_throttle = Throttle::new();

    // 3. El Loop Principal (Escucha 2 cosas al mismo tiempo gracias a tokio::select)
    loop {
        tokio::select! {
            // A. Escucha EVENTOS DE NIRI (Aquí irá la lógica del autofill en el futuro)
            event = event_rx.recv() => {
                if let Some(event) = event {
                    match event {
                        Event::WindowClosed { .. } | Event::WindowLayoutsChanged { .. } => {
                            // Ejecuta el autofill SOLO si pasaron 150ms desde el último salto.
                            // Esto es idéntico al mecanismo "Throttle" de Piri para evitar bucles
                            // infinitos y congelamientos cuando se dispara una ráfaga de eventos.
                            if autofill_throttle.check_and_update(std::time::Duration::from_millis(150)) {
                                let _ = tokio::task::spawn_blocking(|| {
                                    if let Err(e) = auto_fill() {
                                        eprintln!("Error en autofill: {}", e);
                                    }
                                }).await;
                            }
                        }
                        _ => {}
                    }
                }
            }

            // B. Escucha TUS COMANDOS IPC (Aquí irá la lógica de Nirius)
            result = ipc_listener.accept() => {
                if let Ok((mut stream, _)) = result {
                    let mut buffer = String::new();
                    if stream.read_to_string(&mut buffer).await.is_ok() {
                        if let Ok(msg) = serde_json::from_str::<IpcMessage>(&buffer) {
                            match msg {
                                IpcMessage::Fetch { app_id, spawn_cmd, include_current_workspace, focus } => {
                                    // B. Lógica de clonada de Nirius (Fetch window)
                                    let _ = tokio::task::spawn_blocking(move || {
                                        if let Err(e) = fetch_window_logic(&app_id, &spawn_cmd, include_current_workspace, focus) {
                                            eprintln!("Error al buscar/abrir ventana: {}", e);
                                        }
                                    }).await;
                                    stream.write_all(b"Command processed").await.ok();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// Mecanismo antidestrucción / anti-bucles infinitos (el mismo que usa Piri)
struct Throttle {
    last_execution: Option<std::time::Instant>,
}

impl Throttle {
    fn new() -> Self {
        Self { last_execution: None }
    }

    fn check_and_update(&mut self, duration: std::time::Duration) -> bool {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_execution {
            if now.duration_since(last) < duration {
                // Actualiza igual para castigar llamadas demasiado seguidas (debounce)
                self.last_execution = Some(now);
                return false;
            }
        }
        self.last_execution = Some(now);
        true
    }
}

// Función que acomoda las ventanas a la derecha simulando un FocusColumnFirst -> FocusWindow
fn auto_fill() -> Result<()> {
    let mut socket = niri_ipc::socket::Socket::connect()?;

    // 1. Preguntamos qué ventana tiene el foco actualmente
    let focused_id = socket
        .send(Request::FocusedWindow)
        .ok()
        .and_then(|reply| match reply {
            Reply::Ok(Response::FocusedWindow(Some(w))) => Some(w.id),
            _ => None,
        });

    // 2. Obligamos a Niri a mirar a la primera columna (resetea el viewport)
    let _ = socket.send(Request::Action(Action::FocusColumnFirst {}));

    // 3. Devolvemos el foco a donde estábamos (o a la última si la nuestra se cerró)
    let action = if let Some(id) = focused_id {
        Action::FocusWindow { id }
    } else {
        Action::FocusColumnLast {}
    };

    let _ = socket.send(Request::Action(action));
    Ok(())
}

// Función para buscar una ventana por app_id y traerla al workspace actual
// Implementación 1:1 de `move-to-current-workspace-or-spawn` de Nirius.
fn fetch_window_logic(
    target_app_id: &str,
    spawn_cmd: &[String],
    include_current_workspace: bool,
    focus: bool,
) -> Result<()> {
    let mut socket = niri_ipc::socket::Socket::connect()?;

    // 1. Obtener información general de Niri (Ventanas y Workspaces)
    let windows = match socket.send(Request::Windows) {
        Ok(Ok(Response::Windows(w))) => w,
        _ => anyhow::bail!("No se pudo obtener la lista de ventanas"),
    };

    let workspaces = match socket.send(Request::Workspaces) {
        Ok(Ok(Response::Workspaces(ws))) => ws,
        _ => anyhow::bail!("No se pudo obtener la lista de workspaces"),
    };

    let focused_ws_id = match workspaces.iter().find(|ws| ws.is_focused) {
        Some(ws) => ws.id,
        _ => anyhow::bail!("No se encontró un workspace enfocado"),
    };

    // 2. Compilar el Regex para buscar la ventana exacta tal como hace Nirius
    let re = regex::RegexBuilder::new(target_app_id)
        .case_insensitive(true)
        .build()?;

    // 3. Buscar la ventana destino respetando las reglas de Nirius
    println!("Buscando ventana que coincida con: {}", target_app_id);
    let target_window = windows.iter().find(|w| {
        // En Nirius, si no se especifica --include-current-workspace,
        // solo se buscan ventanas de workspaces NO enfocados.
        if !include_current_workspace && w.workspace_id == Some(focused_ws_id) {
            return false;
        }

        // Nirius compara el app_id con el regex proporcionado
        if let Some(ref app_id) = w.app_id {
            if re.is_match(app_id) {
                println!("Match encontrado por App ID: {} (ID: {})", app_id, w.id);
                return true;
            }
        }

        // Fallback: Si no coincide por app_id, permitimos que coincida por titulo
        if let Some(ref title) = w.title {
            if re.is_match(title) {
                println!("Match encontrado por Título: {} (ID: {})", title, w.id);
                return true;
            }
        }

        false
    });

    if let Some(w) = target_window {
        let win_id = w.id;
        println!("Trayendo ventana {} al workspace {}", win_id, focused_ws_id);
        // 4. Mover la ventana a nuestro workspace local
        let _ = socket.send(Request::Action(Action::MoveWindowToWorkspace {
            window_id: Some(win_id),
            reference: niri_ipc::WorkspaceReferenceArg::Id(focused_ws_id),
            focus,
        }))?;

        // 5. Nirius hace un segundo focus explícito si se pide --focus
        if focus {
            let _ = socket.send(Request::Action(Action::FocusWindow { id: win_id }))?;
        }
    } else {
        // 6. Si la ventana NO existe, aplicamos la regla "OrSpawn".
        // MUY IMPORTANTE: Usamos el IPC de Niri para spawnear, igual que Nirius.
        if !spawn_cmd.is_empty() {
            println!("No match, spawning via Niri: {:?}", spawn_cmd);
            let _ = socket.send(Request::Action(Action::Spawn {
                command: spawn_cmd.to_vec(),
            }))?;
        }
    };

    Ok(())
}
