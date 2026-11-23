# Medidas de referencia (upstream sin modificar)

- Equipo: i5-12400F (12 hilos), 32 GB de RAM, RTX 3070, Windows 11 Pro 26300.
- Build: `cargo build --release -j 4 --bin warp-oss --features gui`, con `debug=0` en el crate `warp`. Commit `0356d03b`.
- Ejecutable: `warp-oss.exe`, de 339 MB.
- Compilación: 25 min las dependencias y 13,5 min el crate `warp`. Con el paralelismo por defecto (12) y debuginfo, rustc se queda sin memoria compilando `warp` (32 GB).

| Escenario | WS medio | WS máx. | Privada máx. | CPU media | CPU máx. | Procesos | Hilos | Fecha |
|---|---|---|---|---|---|---|---|---|
| Ventana hasta visible | | | | | | | | 0,67 s |
| Reposo, 1 ventana recién abierta, 5 min | 1.227 MB | 1.232 MB | 977 MB | 0,11 % | 4,2 % | 4 | 83 | 2026-11-19 |

## Desglose de memoria con VMMap (pantalla de bienvenida)

Fichero: `measurements/vmmap-bienvenida.csv`

| Tipo | WS total | WS privado | Qué es |
|---|---|---|---|
| Heap | 627 MB | 627 MB | Reservas de memoria de Rust |
| Mapped File | 304 MB | 0 (compartible) | ~300 MB son ficheros de `C:\Windows\Fonts` mapeados: CJK, emoji… |
| Private Data | 167 MB | 166 MB | VirtualAlloc: driver de la GPU, buffers |
| Image | 148 MB | 7 MB | El exe y las DLL |
| **Total** | **1.255 MB** | **802 MB** | |

Causas que ya he localizado en el código:
1. `app/src/lib.rs:2139`: `search_for_settings_to_import` se ejecuta en **cada arranque** y llama a `all_system_fonts`, que carga **todas** las fuentes del sistema. En este equipo son 455 ficheros y unos 593 MB. En Windows solo hay un importador (Alacritty), y además está desactivado por el flag `AlacrittySettingsImport`. Es candidata a explicar la mayor parte de los 627 MB de heap.
2. `crates/warpui_core/src/core/app.rs:873`: `prewarm_svg_font_db` hace `fontdb.load_system_fonts()` al arrancar, que mapea todas las fuentes del sistema para renderizar SVG. Explica los ~300 MB de Mapped File.

## Rama `light/fonts-startup` (2026-11-19)

| Escenario | WS medio | Privada máx. | CPU media | Heap WS | Mapped WS |
|---|---|---|---|---|---|
| Upstream, reposo 5 min | 1.227 MB | 977 MB | 0,11 % | 627 MB | 304 MB |
| Con los dos cambios, reposo 5 min | **920 MB** | 962 MB | 0,25 % | 621 MB | **2 MB** |

- Quitar la precarga de fuentes para SVG funciona: el Mapped File pasa de 304 a 2 MB y el WS total baja unos 307 MB (−25 %).
- Saltar la importación de configuración **no cambia el heap**. Esos ~620 MB no vienen de ahí; hace falta un perfilador de heap para saber de dónde salen.

## Perfil de heap con dhat (`--features dhat_heap_profiling`)

Heap en el pico: 583 MB. De ellos, **572 MB (98 %)** salen de `AgentOnboardingView::preload_onboarding_images`. Esa función decodifica a RGBA unas 45 capturas PNG de onboarding, de hasta 1920×2388 píxeles, al iniciar la bienvenida, y las deja en la caché de assets.

| Escenario | WS medio | Privada máx. | CPU media |
|---|---|---|---|
| Upstream, bienvenida | 1.227 MB | 977 MB | 0,11 % |
| Sin precarga de fuentes | 920 MB | 962 MB | 0,25 % |
| **Sin precarga de fuentes ni de imágenes de onboarding** | **283 MB** | **389 MB** | 0,23 % |

Total: **−77 % de WS y −60 % de memoria privada** en la pantalla de bienvenida.

En el reposo, el proceso principal ocupaba 1.197 MB de WS y 967 MB de memoria privada. El segundo `warp-oss` (terminal server) ocupaba 13,5 MB, y había además 2 `conhost`. No aparece ningún proceso de shell en el árbol.

## Terminal en reposo (2026-11-20)

Medido con la bienvenida ya completada: una pestaña de PowerShell en reposo durante 5 min.

### Perfil de heap con dhat

Heap en el pico: 147 MB. Al cerrar: 27 MB.

| Sitio | MB en el pico | Causa |
|---|---|---|
| `piper::pipe` vía `async_process::Child::output` | **96** | En Windows, `async-process` lee stdout y stderr con `blocking::Unblock`, que crea un pipe de **8 MB por flujo**. Ocurre en cada comando en segundo plano: historial (`read_history_contents`), prompt (`context_chips`), `load_deferred_name_set`… |
| `tantivy::IndexWriter::recreate_document_channel` | 23 | Cola del índice de búsqueda; se recrea en cada commit. Pendiente. |
| Resto | ~28 | Fuentes, menús, expresiones regulares… |

Arreglo: `command::r#async::Command::spawn_for_output()` y `output()` leen la salida en Windows con `std::io::pipe` y un hilo bloqueante, sin el buffer de 8 MB. `LocalCommandExecutor` usa `spawn_for_output()`.

### Resultado

| Escenario | WS árbol | Proceso principal: WS | Proceso principal: privada | Heap WS (VMMap) | CPU media |
|---|---|---|---|---|---|
| Antes (`9c431ed3`) | 697 MB | 486 MB | 561 MB | 212 MB | 0,03 % |
| **Con el arreglo de pipes** | **510 MB** | **317 MB** | **417 MB** | **64 MB** | 0,40 % |

- El PowerShell que corre dentro ocupa 150 MB de WS en ambos casos; no es memoria de Warp.
- La CPU media sube un poco (0,40 %), pero sigue siendo despreciable. Habrá que vigilarla en la prueba larga.

Nota sobre los tests: el sandbox de Claude Code ejecuta los comandos dentro de un Job Object que no permite `CREATE_BREAKAWAY_FROM_JOB`. Por eso `Command::new` falla con «Acceso denegado» y los tests de procesos de `command` hay que lanzarlos fuera del job, por ejemplo con `Win32_Process.Create` vía WMI. Así pasan los 8.

## Fuga: las pestañas cerradas no se liberaban (2026-11-20 / 2026-11-23)

Síntoma: cada 30 s, Warp lanzaba ráfagas de 20–30 procesos (`powershell.exe -c "git symbolic-ref…"` y `git diff --shortstat`), con picos de más de 1 GB. Eran los chips de git del prompt (`GIT_REFRESH_CONFIG`, `app/src/context_chips/mod.rs`). Había **una copia por cada pestaña cerrada en la sesión**: tras abrir y cerrar 3 pestañas, las copias pasaron de 10 a 14, y siguieron así más de 4 min después de cerrar (el margen del "deshacer cierre" es de 60 s).

Diagnóstico: `LIGHT_WARP_ENTITY_DUMP=<fichero>` escribe cada 10 s el número de modelos y vistas vivos por tipo (`AppContext::maybe_dump_entity_counts`). Tras cerrar 3 pestañas, las vistas se liberaban, pero quedaban 3 copias de 10 modelos por terminal: `Sessions`, `CurrentPrompt`, `PromptType`, `InputBufferModel`…

Causa: WarpUI guarda cada suscripción u observación en la lista del emisor. Al morir el suscriptor, su callback solo se descartaba la siguiente vez que el emisor emitía. Con emisores singleton que casi nunca emiten, los callbacks (y los `ModelHandle` que capturan) vivían para siempre.

Arreglo: `remove_dropped_items` purga de todas las listas las suscripciones y observaciones de los modelos y vistas liberados. Test: `test_dropping_subscribers_releases_handles_captured_by_their_callbacks` (falla sin el arreglo).

Resultado tras abrir y cerrar 3 pestañas:

| | Antes | Después |
|---|---|---|
| Tipos de modelo retenidos por cada pestaña cerrada | 10 | 4 |
| `CurrentPrompt` (sondeo de git cada 30 s) | retenido | liberado |

Pendiente: siguen retenidos `BlocklistAIContextModel` → `ConversationSelection` → `AgentViewController` → `EphemeralMessageModel` (1 de cada por pestaña). No es un ciclo directo entre ellos; falta encontrar qué retiene `BlocklistAIContextModel`.
