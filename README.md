# Renuevo

App de escritorio en Rust para **regenerar fotos píxel a píxel sin perder calidad** y, si se
quiere, **restaurarlas** (ruido, bloques JPEG) y **ampliarlas** con algoritmos clásicos, sin IA.

## Qué hace

| Modo | Qué cambia | Garantía |
|---|---|---|
| **Copia exacta** | Nada: reescribe cada píxel en PNG/TIFF/WebP sin pérdida | Al guardar se vuelve a leer el archivo y se compara **cada píxel** con el original. Si algo no cuadra, no se escribe |
| **Restaurar** | Bloques JPEG → ruido de color → grano (NLM) → enfoque con umbral | Se adapta al ruido que mide en la foto; en fotos limpias casi no actúa |
| **Ampliar** | ×1,5 / ×2 / ×3 / ×4 con Lanczos3 o EASU («bordes nítidos») + RCAS | Salida en 16 bits para no crear bandas |

- **Formatos de entrada**: JPEG, PNG, WebP, TIFF y **HEIC** (iPhone), incluido HEIC de 10 bits sin truncar.
- **Metadatos**: se **conserva el perfil de color** (ICC, o NCLX de HEIC convertido a ICC: Display P3 sigue
  siendo Display P3) y se **elimina el EXIF** (GPS, cámara, fecha). La orientación del EXIF se aplica a
  los píxeles antes de quitarlo, para que la foto no salga girada. Los **DPI** se conservan en PNG y TIFF
  (al ampliar ×2 se duplican, para mantener el tamaño de impresión).
- **Color que no es ICC**: `gAMA`/`cHRM`/`cICP` de PNG se convierten en un ICC equivalente. Los JPEG/TIFF
  **CMYK** se convierten con su propio perfil (y la intención que declara, como ColorSync) a ProPhoto
  RGB en 16 bits, cuya gama contiene toda la de imprenta (PNG/TIFF/WebP no guardan CMYK): se ven igual
  que en macOS (diferencia media 1/255). El alfa premultiplicado (TIFF/HEIC) se pasa a alfa normal.
- **Lotes**: aplica los ajustes a toda una carpeta, de una en una. Nunca sobrescribe un original:
  ninguna salida puede ser (por inodo) una foto de entrada, y `a.jpg` + `a.png` dan
  `a_jpg_renuevo.png` y `a_png_renuevo.png` en lugar de pisarse.
- **Visor**: comparador antes/después con divisor, zoom, vista 1:1 y gestión de color en pantalla. Con
  zoom, los cambios de ajustes se previsualizan solos en la zona visible.

> Ningún algoritmo recupera información que ya se perdió. La copia exacta garantiza que **no se pierde
> nada más**; restaurar y ampliar mejoran lo que se ve, pero no inventan detalle real.

## Requisitos (macOS)

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust estable (≥ 1.95)
brew install libheif pkg-config                                   # solo para HEIC
```

Sin libheif se puede compilar sin HEIC: `cargo build -p renuevo-app --no-default-features`.

## Uso

```sh
cargo run --release -p renuevo-app               # abre la app
cargo run --release -p renuevo-app -- foto.heic  # abre la app con una foto
```

Atajos: **⌘O** abrir · **⌘S** exportar · **F** o doble clic: ajustar · **1**: 100 % ·
pellizco o ⌘+rueda: zoom · arrastrar o rueda: mover · arrastrar la línea blanca: comparar.
También se puede arrastrar una foto (o una carpeta, que abre «Lotes») a la ventana.

## Estructura

```
crates/renuevo-core   biblioteca sin GUI: io (decode/encode/ICC/HEIC), verify, export,
                      restore (deblock, chroma, nlm, noise, sharpen), upscale (lanczos, easu, rcas),
                      pipeline, batch, metrics (PSNR/SSIM)
crates/renuevo-app    GUI con eframe/egui 0.37: visor, paneles, lotes, trabajos en segundo plano
```

## Tests y medidas

```sh
cargo test --workspace                                   # núcleo + GUI (egui_kittest, sin ventana)
cargo run --release -p renuevo-core --example bench      # rendimiento con 12 MP
cargo run --release -p renuevo-core --example quality -- original.png degradada.jpg
RENUEVO_PHOTO=foto.heic RENUEVO_OUT=capturas \
  cargo test --release -p renuevo-app --test render -- --ignored   # capturas reales con GPU
```

Los tests comprueban la ida y vuelta bit a bit (PNG 8/16, TIFF, WebP), el ICC byte a byte, la orientación,
que se elimina el EXIF con GPS, la decodificación HEIC real (generado con `sips` y `heif-enc`: gris, giros,
alfa premultiplicado), CMYK frente al render de macOS, `gAMA`/`cHRM`, los DPI, y que restaurar y
ampliar **mejoran** el PSNR/SSIM frente a la referencia: ruido +7 dB, JPEG con ruido +3,5 dB, ampliar ×2
por encima del bilineal, y que una JPEG limpia no empeora.

Rendimiento en un Apple M5 (10 hilos), foto de 12 MP: verificar la copia exacta 0,06 s · restaurar 1,1 s ·
ampliar ×2 0,3–0,5 s · todo junto 1,5 s.

## Limitaciones conocidas

La app avisa en el panel de la foto de todo lo que no puede copiar:

- **CMYK sin perfil incrustado**: solo queda la conversión ingenua de `image` («los colores pueden
  cambiar»). Con perfil se convierte con gestión de color.
- **HDR**: los mapas de ganancia de las fotos HDR del iPhone (y Ultra HDR / ISO 21496) no caben en
  PNG/TIFF/WebP: se copia la foto normal (SDR) y se avisa.
- **Varias imágenes**: de un TIFF de varias páginas, un PNG/WebP animado o un HEIF con varias imágenes
  solo se copia la primera (o la principal), y se avisa.
- **WebP** no guarda DPI.
- **TIFF poco habituales** que no se pueden abrir (con un error claro, nunca con colores erróneos): gris
  con alfa, CMYK con canal alfa y CMYK en planos separados. Tampoco los JPEG CMYK con la K a más
  resolución que C, M y Y (el decodificador los leería mal).
- Un **JPEG cortado** se abre con la parte que falta en gris, y se avisa.

## Licencias

Código propio bajo MIT. EASU y RCAS son ports de AMD FidelityFX FSR 1 (MIT): ver
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
