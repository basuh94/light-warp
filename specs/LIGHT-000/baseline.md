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
