# Plan light-warp

Objetivo: un Warp de escritorio para **Windows** que sea ligero en RAM y CPU, que funcione **sin la nube de Warp** y con un agente que use **modelos de Ollama**.

## Decisiones

| Tema | Decisión |
|---|---|
| Plataforma | Solo Windows (x86_64-pc-windows-msvc). Se eliminan los backends de macOS (Metal/ObjC), Linux y wasm. |
| Modelos | Ollama, con su API nativa `/api/chat` para poder controlar `num_ctx`. La llamada va detrás de un trait para poder añadir otros proveedores. |
| Front-end | Solo escritorio (GUI). Se elimina la consola (`crates/warp_tui`, `app/src/tui*`, la feature `tui` y los elementos TUI de `warpui_core`). |
| Drive | Se elimina por completo. Los workflows se guardan solo en local (YAML). Los notebooks se eliminan. |
| Sincronización con Warp | Fork independiente: no se sigue a upstream. Las correcciones de la terminal que interesen se traen a mano. |
| Nube | Se elimina todo: auth/Firebase, GraphQL, websocket, telemetría, Sentry, autoupdate, billing, equipos, session sharing, Oz y los agentes en la nube. |

## Principios

1. Primero medir y después optimizar. Cada optimización tiene que justificarse con una medida.
2. Primero desenchufar y después borrar. Cuando la app funcione sin servidor, se borra el código de nube siguiendo los errores del compilador.
3. No tocar la interfaz ni la ejecución de herramientas del agente. Solo se cambia de dónde salen las respuestas del modelo.

## Fase 0 · Base de trabajo

- [ ] Instalar la toolchain: Rust 1.92, VS 2022 Build Tools (VCTools + Windows 11 SDK 22621), CMake, Protobuf y LLVM. No hace falta gcloud ni InnoSetup.
- [ ] Compilar `cargo build --release --bin warp-oss --features gui`.
- [ ] Medir la referencia (ver la tabla de abajo) y guardar los resultados en `specs/LIGHT-000/baseline.md`.
- [ ] Mover a la organización propia las dependencias git de `warpdotdev`: winit, vte, font-kit, notify, jemallocator, tink-rust, pathfinder, rust-objc, yaml-rust, difflib, rust-email_address, command-signatures, command-corrections, workflows, mermaid-to-svg, session-sharing-protocol y warp-proto-apis.
- [ ] Desactivar los workflows de `.github/` de Warp y crear una CI mínima en `windows-latest`: build, clippy y nextest.
- [ ] Quitar de `script/run` la descarga remota de `common-skills`.

### Medidas de referencia

| Escenario | RAM (working set / privada) | CPU | Notas |
|---|---|---|---|
| Arranque en frío, hasta la primera ventana | | | tiempo |
| En reposo, 1 pestaña, 5 min | | | ¿CPU distinta de 0 en reposo? |
| En reposo, 10 pestañas | | | |
| `yes` durante 30 s | | | throughput |
| `cat` de un fichero de 200 MB | | | |
| Historial de 100k líneas | | | |
| 8 h abierta con uso normal | | | ¿la memoria crece sin parar? |
| Procesos hijos | | | número y RAM de cada uno |

Herramientas: Windows Performance Recorder/Analyzer (CPU y heap), VMMap, Process Explorer y la feature `pprof_cpu_profiling` del propio repo.

## Fase 1 · Desenchufar: funcionar 100% sin la nube

- [ ] Modo local fijo: un usuario local permanente, sin pantalla de login, y quitar el paso por `attempt_login_gated_feature`.
- [ ] Sustituir `ServerApi` por una implementación vacía que no hace ninguna llamada de red.
- [ ] Apagar los servicios de nube en segundo plano: el sondeo de Drive y el websocket RTC, los experimentos, la telemetría (convertida en no-op), Sentry, la autoactualización, `cloud_preferences_syncer` y session sharing.
- [ ] Workflows solo locales (`LocalWorkflows`).
- [ ] Verificar que funciona con un firewall que bloquee todo el tráfico saliente, salvo `localhost:11434` (Ollama).

## Fase 2 · Agente con Ollama

- [ ] Crear un crate `light_llm` con un trait `ModelProvider` y una implementación `OllamaProvider` (`/api/chat`, streaming y tools).
- [ ] Sustituir `generate_multi_agent_output` (`app/src/ai/agent/api/impl.rs:16`) por un bucle local que emita los mismos `ResponseEvent`.
- [ ] Escribir el prompt de sistema y las definiciones de herramientas (pasar `AIAgentActionType` a JSON schema).
- [ ] Convertir la conversación en mensajes y gestionar la ventana de contexto (recortar o resumir).
- [ ] Configuración de modelos en `settings.toml`: `base_url`, modelo, `num_ctx` y temperatura, y un selector de modelo en la interfaz.
- [ ] Quitar el bloqueo de localhost e IPs privadas en `app/src/ai/custom_endpoints.rs`.
- [ ] Entregar las herramientas por orden: (1) comandos, lectura y edición de ficheros, grep/glob → (2) MCP y skills → (3) subagentes → (4) sugerencias, lenguaje natural a comando y títulos de bloque.
- [ ] Mantener los agentes de terceros por ACP (Claude Code, Codex y Gemini).
- [ ] Elegir el modelo de referencia probando con la RTX 3070 (8 GB de VRAM): `qwen2.5-coder:7b/14b`, `qwen3:8b/14b`, `llama3.1:8b`.

## Fase 3 · Borrar la nube y lo que sobra

- [ ] La consola: `crates/warp_tui`, `app/src/tui*`, la feature `tui` y `warpui_core/src/elements/tui` y `runtime/`.
- [ ] Los backends que no son de Windows: `warpui/src/platform/mac`, Metal/ObjC, Linux, wasm, `serve-wasm` y `managed_secrets_wasm`.
- [ ] Auth y Firebase: `app/src/auth`, `crates/warp_server_auth`, `crates/firebase`.
- [ ] La comunicación con el backend: `crates/graphql`, `warp_graphql_schema`, `websocket`, `warp_server_client` y `app/src/server`.
- [ ] Drive y objetos en la nube: `app/src/drive`, `app/src/cloud_object`, `crates/cloud_object*`, `crates/cloud_objects` y `notebooks/`.
- [ ] Cuentas y negocio: billing, pricing, usage, `workspaces/` (equipos), referidos y experimentos.
- [ ] Servicios de Warp: telemetría, `crash_reporting`, `autoupdate` y session sharing.
- [ ] Agentes en la nube: `ambient_agents`, `cloud_environments`, `agent_management`, `handoff`, Oz/`agent_sdk` en la nube, `managed_secrets`, `isolation_platform`, Factory MCP y el `remote_tty` del cloud shell.
- [ ] Dejar un solo canal y eliminar `warp_channel_config` y los binarios `stable`, `preview`, `dev` y `local`.
- [ ] Fijar o eliminar los feature flags que ya no aplican y bajar las features de Cargo de `default`.
- [ ] Una migración de SQLite que elimine las tablas de nube (`object_*`, `teams`, `team_*`, `workspaces`, `cloud_objects_refreshes`, `server_experiments`, `notebooks` y otras).

## Fase 4 · Optimizar memoria y CPU (guiado por las medidas de la fase 0)

Candidatos:
- [ ] Cargar en diferido los ~138 singletons de `initialize_app` (LSP, índice de código, árbol de ficheros, tantivy, MCP y voz).
- [ ] Limitar o desactivar por defecto los watchers: `HomeDirectoryWatcher`, `DetectedRepositories` y los watchers por repositorio.
- [ ] Clasificador de lenguaje natural: solo el heurístico, o cargar el modelo ONNX bajo demanda.
- [ ] Terminal: `MaximizeFlatStorage`, un límite configurable de historial, menos bloques restaurados al arrancar y no guardar la salida con estilos de los bloques antiguos.
- [ ] Render: redibujar solo cuando algo cambie, no dejar animaciones en reposo, reducir el tiempo que el render mantiene el lock del modelo y revisar el atlas de glifos y el backend DX12 frente a Vulkan.
- [ ] Procesos hijos que se lanzan solo cuando hacen falta.
- [ ] Perfil de release con LTO completo, `codegen-units=1` y probar mimalloc.
- [ ] Prueba de 24 h para detectar memoria que crece sin parar.

## Fase 5 · Independencia

- [ ] Instalador propio (`script/windows/bundle.ps1` + InnoSetup) y firma del código.
- [ ] Actualizaciones desde GitHub Releases o un servidor interno.
- [ ] Cambiar marca e iconos si se distribuye fuera del equipo.
- [ ] Revisión legal de la AGPL si se distribuye.
- [ ] Sustituir `warp_multi_agent_api` (los protos de Warp) por tipos propios.

## Riesgos

- La calidad del agente depende del prompt y de las definiciones de herramientas, que hoy escribe Warp en su servidor. Con modelos locales de 7B–14B, el uso de herramientas es menos fiable.
- El código de nube está muy mezclado con el resto: `terminal/view.rs` y `workspace/view.rs` tienen unas 30.000 líneas cada uno. Hacen falta tests de regresión de la terminal antes de borrar.
- El lock global de `TerminalModel` puede congelar la interfaz si se toca sin cuidado.
