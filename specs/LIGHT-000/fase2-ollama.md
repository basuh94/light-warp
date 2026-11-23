# Fase 2: agente local con Ollama (diseño)

## Cómo funciona hoy

El modo agente de Warp envía cada turno a `POST /ai/multi-agent` (protobuf `warp.multi_agent.v1`, repo `warp-proto-apis`, rev `7e78c26`) y recibe un flujo SSE de `ResponseEvent`. El punto de entrada en el cliente es `app/src/ai/agent/api/impl.rs` (`generate_multi_agent_output`).

**Petición** (`request.proto`):
- `task_context.tasks`: la conversación entera, que guarda el cliente. Cada `Task` tiene una lista de `Message`.
- `input`: lo nuevo del turno, normalmente `user_inputs` con `UserQuery` (la pregunta) o `ToolCallResult` (el resultado de una herramienta que el cliente acaba de ejecutar).
- `settings`: modelo, herramientas soportadas, permisos…

**Respuesta** (`response.proto`), un flujo de:
1. `StreamInit { conversation_id, request_id }`, una vez al principio.
2. `ClientActions`: `CreateTask`, `AddMessagesToTask`, `AppendToMessageContent` (texto incremental), `BeginTransaction` y `CommitTransaction`…
3. `StreamFinished { Done | InternalError | LLMUnavailable | … }`, una vez al final.

**Mensajes** (`task.proto`, `Message.oneof`): `UserQuery`, `AgentOutput { text }`, `AgentReasoning`, `ToolCall { tool_call_id, oneof tool }` y `ToolCallResult { tool_call_id, oneof result }`, entre otros. Ejemplo: `ToolCall.RunShellCommand { command, risk_category, wait_until_complete }` produce `RunShellCommandResult { command, command_finished { output, exit_code } | permission_denied | … }`.

**Consecuencia clave:** el servidor no guarda estado. Cada petición trae todo el historial, así que un proveedor local puede reconstruir la conversación en cada turno sin base de datos propia.

**Lo que bloquea hoy usar la IA:** `AISettings::is_any_ai_enabled` (`app/src/settings/ai.rs`) la desactiva si el usuario no ha iniciado sesión.

## Diseño

### 1. Crate `crates/light_llm`
Cliente de Ollama sin dependencias de Warp:
- `POST /api/chat` con `stream: true` (NDJSON), `tools` (JSON Schema), `options.num_ctx` y `think` opcional.
- Tipos `ChatMessage { role, content, tool_calls }` y eventos `ChatEvent::{ContentDelta, ThinkingDelta, ToolCall, Done { prompt_eval_count, eval_count }, Error}`.
- `GET /api/tags` para listar los modelos instalados.
- Tests con un servidor HTTP falso que devuelve NDJSON grabado.

### 2. Módulo `app/src/ai/agent/local/`
`generate_local_agent_output(params, cancellation_rx) -> ResponseStream`, con la misma firma que hoy. `impl.rs` lo llama en lugar del servidor cuando el proveedor local está configurado.

- **De Warp a Ollama:** prompt de sistema, luego los mensajes de la tarea raíz en orden (`UserQuery` → `user`, `AgentOutput` → `assistant`, `ToolCall` → `assistant.tool_calls`, `ToolCallResult` → `tool`) y, al final, las entradas nuevas del turno.
- **De Ollama a Warp:**
  - `StreamInit`: reutiliza el `conversation_id` o genera un UUID.
  - `CreateTask`: solo en el primer turno.
  - `AddMessagesToTask`: con las entradas del turno, el mensaje `AgentOutput` vacío y cada `ToolCall`.
  - `AppendToMessageContent` (máscara `agent_output.text`): por cada trozo de texto.
  - `StreamFinished::Done`, o `InternalError` / `LLMUnavailable` si Ollama falla o no responde.
- **Cancelación:** `take_until(cancellation_rx)`, como el código actual.

### 3. Herramientas (por etapas)
1. `run_shell_command`. Siempre requiere confirmación del usuario mientras no haya clasificación de riesgo.
2. `read_files`, `grep` y `file_glob_v2`, que son de solo lectura.
3. `apply_file_diffs` y `create_documents`, que editan ficheros.

Cada herramienta tiene su definición JSON Schema para Ollama y su conversión a `ToolCall` y desde `ToolCallResult`. El cliente ya sabe ejecutarlas y mostrarlas, porque esa parte es local en Warp.

### 4. Prompt de sistema
Escrito por nosotros, porque el de Warp vive en sus servidores. Contenido: rol, sistema operativo y shell (PowerShell en Windows), directorio actual, reglas de uso de herramientas, y "pide confirmación antes de cambios destructivos".

### 5. Ajustes y activación
- Ajustes nuevos: `ai.local_llm.base_url` (por defecto `http://127.0.0.1:11434`), `ai.local_llm.model` (por defecto `qwen3:8b`) y `ai.local_llm.num_ctx` (por defecto 16384).
- `is_any_ai_enabled`: permitir la IA sin sesión cuando el proveedor local está configurado.
- Selector de modelos: los modelos instalados en Ollama (`/api/tags`), en lugar de la lista del servidor.

## Riesgos y validaciones
- **Comprobar en el cliente si espera que el servidor reenvíe la `UserQuery` como mensaje de la tarea.** Se valida en la primera prueba: si la pregunta del usuario aparece dos veces o no aparece, se ajusta.
- **Transacciones** (`BeginTransaction` / `CommitTransaction`): se empieza sin ellas y se añaden si el cliente las necesita para confirmar los mensajes.
- **Modelos pequeños y llamadas a herramientas:** `qwen3:8b` las soporta, pero con menos fiabilidad que un modelo grande. El prompt debe ser corto y claro.
- **Contexto:** con 8 GB de VRAM, `num_ctx` 16k es un límite razonable. Habrá que recortar la salida de los comandos largos antes de enviarla.

## Orden de trabajo
1. `light_llm` con tests.
2. Conversión Warp ↔ Ollama con tests (texto solamente).
3. Conectarlo en `impl.rs` y en `is_any_ai_enabled`. Primera prueba real: conversación de texto con `qwen3:8b`.
4. `run_shell_command` de principio a fin.
5. Herramientas de lectura, después las de edición.
6. Ajustes y selector de modelos.

## Estado (2026-11-23)

Paso 1 hecho: conversación de texto con `qwen3:8b` dentro de Warp.
- `crates/light_llm`: cliente de Ollama (`/api/chat` en streaming NDJSON, `num_ctx`, herramientas, razonamiento, `/api/tags`). 7 tests.
- `app/src/ai/agent/api/local.rs`: traduce la tarea raíz al historial de chat; emite `StreamInit`, `CreateTask` (solo en el primer turno), el eco de la `UserQuery`, `AgentReasoning` y `AgentOutput` en streaming, y `StreamFinished`. Las peticiones que no son un turno del usuario (p. ej. sugerencias pasivas) terminan sin llamar al modelo. 6 tests.
- `generate_multi_agent_output` lo usa cuando `ChannelState::is_offline()`, y `is_any_ai_enabled` no exige sesión en ese caso.
- Verificado a mano: la pregunta aparece una vez, el razonamiento se ve aparte y la conversación mantiene el contexto entre turnos.
- Configuración provisional por variables de entorno: `LIGHT_WARP_OLLAMA_URL`, `LIGHT_WARP_OLLAMA_MODEL` y `LIGHT_WARP_OLLAMA_NUM_CTX`.

Pendiente detectado: el ajuste guardado `agents.warp_agent.is_any_ai_enabled` se quedó en `false` al saltarse el login en la bienvenida, y hubo que activarlo a mano en `settings.toml`. En la build sin conexión debe venir activado y poder activarse desde la interfaz.

## Paso 2: configuración desde Warp (2026-11-23)

El proveedor local es opcional: nada contacta con un servidor de modelos hasta que el usuario lo activa.
- Ajustes nuevos en `agents.local_model`: `enabled` (por defecto `false`), `url` (por defecto `http://127.0.0.1:11434`), `model` y `context_window` (16384).
- **Ajustes → Agente → "Local model"** (`settings_view/local_model_settings.rs`): interruptor, dirección del servidor, "Detect models" con el estado de la conexión, lista de modelos para elegir y tamaño de contexto.
- **Sin configurar:** el agente responde con un `InvalidApiKey`, que en la build sin conexión se muestra como "No local model configured" con un botón "Configure local model" que abre esa sección.
- **Configurado:** el catálogo del selector de modelos se rellena con los modelos instalados (`LLMPreferences::refresh_local_models`), reintentando cada 30 s mientras el servidor no responda. Se recarga al cambiar los ajustes. El modelo elegido en el selector tiene prioridad sobre el de Ajustes.
- La bienvenida ya no desactiva la IA al elegir el modo agente sin cuenta, en la build sin conexión.

Verificado a mano: aviso y botón, activación, detección de modelos, selección y respuesta del modelo.

Pendiente: los tests del crate `warp` (`local_models`, `agent::api::local`) no se han podido pasar tras estos cambios. Compilar los tests de `warp` necesita más de 17 GB, y el watchdog los detiene con las aplicaciones habituales abiertas, tanto en debug como en release. El ejecutable release sí compila.
