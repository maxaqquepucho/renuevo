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
- **Metadatos**: la copia es un archivo nuevo que lleva solo los píxeles, el **perfil de color** (ICC, o
  NCLX de HEIC convertido a ICC: Display P3 sigue siendo Display P3) y los **DPI** (en PNG y TIFF; al
  ampliar ×2 se duplican, para mantener el tamaño de impresión). Todo lo demás se **elimina**: EXIF
  (GPS, cámara, fecha), XMP, IPTC, Content Credentials (C2PA), textos de PNG (como el prompt de una
  imagen generada con IA), miniaturas y MakerNotes. Eso incluye la etiqueta de «generada con IA» que
  llevan C2PA o XMP. La orientación del EXIF se aplica a los píxeles antes de quitarlo, para que la foto
  no salga girada. Los perfiles que sintetiza la app llevan siempre la misma fecha, así que exportar
  dos veces la misma foto da archivos idénticos byte a byte.
- **Color que no es ICC**: `gAMA`/`cHRM`/`cICP` de PNG se convierten en un ICC equivalente (`cICP` manda
  sobre `iCCP`, como en macOS y en la especificación PNG). En HEIC, un NCLX con curva de vídeo
  (BT.709/BT.601) se trata como sRGB, que es como lo muestra macOS. Los JPEG/TIFF
  **CMYK** se convierten con su propio perfil (y la intención que declara, como ColorSync) a ProPhoto
  RGB en 16 bits, cuya gama contiene toda la de imprenta (PNG/TIFF/WebP no guardan CMYK): se ven igual
  que en macOS (diferencia media 1/255). El alfa premultiplicado (TIFF/HEIC) se pasa a alfa normal.
- **Compresión PNG**: rápida, equilibrada o **máxima con [oxipng](https://github.com/oxipng/oxipng)**.
  La máxima deja las fotos en torno a un 10 % más pequeñas que la equilibrada (hasta un 25 % en HEIC
  de 10 bits) y nunca más grandes, pero tarda varias veces más: una foto de 24 MP, de 6 a 10 s en un
  M5. Solo cambia cómo se comprime: mismo tipo de píxel y profundidad, mismo perfil y mismos DPI, y se
  verifica igual. Por encima de ~180 MP en 8 bits (~90 MP en 16 bits) oxipng necesitaría más de 4 GB
  de memoria y se usa la compresión máxima de `image`.
- **Lotes**: aplica los ajustes a toda una carpeta, de una en una. Nunca sobrescribe un original:
  ninguna salida puede ser (por inodo) una foto de entrada, y `a.jpg` + `a.png` dan
  `a_jpg_renuevo.png` y `a_png_renuevo.png` en lugar de pisarse.
- **Visor**: comparador antes/después con divisor, zoom, vista 1:1 y gestión de color en pantalla (en
  16 bits, con la intención que declara el perfil, también para perfiles de gris). Las fotos HDR (PQ/HLG)
  se muestran con un mapeo de tonos parecido al de macOS en una pantalla normal. Con zoom, los cambios
  de ajustes se previsualizan solos en la zona visible.

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

### Como app de macOS

```sh
scripts/bundle-macos.sh              # crea target/release/Renuevo.app (con HEIC)
scripts/bundle-macos.sh --sin-heic   # sin HEIC: más pequeña y declara macOS 11 como mínimo
rm -rf /Applications/Renuevo.app     # la anterior, si la hay: copiar encima mezcla las dos
cp -R target/release/Renuevo.app /Applications/
```

La app lleva dentro libheif y las librerías de Homebrew que usa (con sus licencias en
`Contents/Resources/Licencias`), así que funciona aunque luego se desinstale Homebrew, y no carga
los plugins de libheif que pueda haber en Homebrew (`libheif-plugins`). Pide la misma
versión de macOS para la que Homebrew compiló esas librerías (la de este Mac). Se firma «ad hoc»:
sirve para este Mac. Para repartirla haría falta firmarla con un certificado de Apple y notarizarla, y
tener en cuenta que el x265 que trae libheif es GPL. El icono se genera con
`cargo run -p renuevo-app --example make_icon`.

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
  o con secuencia solo se copia la primera (o la principal), y se avisa.
- **WebP** no guarda DPI y admite como máximo 16383 px por lado (se avisa antes de procesar).
- Lo que va **dentro de los propios píxeles** (marcas de agua invisibles, datos escondidos en el alfa)
  se conserva: la copia exacta no cambia ningún píxel.
- **TIFF poco habituales** que no se pueden abrir (con un error claro, nunca con colores erróneos): gris
  con alfa, CMYK con canal alfa, CMYK en planos separados, YCbCr (con submuestreo o JPEG dentro) y Lab.
  Tampoco los JPEG CMYK con la K a más resolución que C, M y Y (el decodificador los leería mal).
- Un **JPEG o HEIC cortado o dañado** se abre igual, con la parte que falta en gris, verde o un color
  plano, y se avisa.
- **Renuevo.app** no recibe archivos desde el Finder («Abrir con» o soltarlos en el icono del Dock):
  winit, la capa de ventanas de egui, no lo ofrece, y añadirlo exige código `unsafe` de AppKit, que el
  proyecto no permite. Se abren con ⌘O o soltándolos en la ventana.

## Licencias

Código propio bajo MIT. EASU y RCAS son ports de AMD FidelityFX FSR 1 (MIT): ver
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), que también lista las licencias de las librerías
que lleva Renuevo.app.
