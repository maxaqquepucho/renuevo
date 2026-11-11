#!/bin/bash
# Crea Renuevo.app junto al binario de Cargo (normalmente en target/release/).
#
#   scripts/bundle-macos.sh             con HEIC: mete dentro libheif y las librerías que usa
#   scripts/bundle-macos.sh --sin-heic  sin HEIC: no necesita Homebrew
#
# La app no depende ni de Homebrew ni de esta carpeta: lleva el binario, el icono y las
# librerías dinámicas que no son del sistema (en Contents/Frameworks, con sus rutas
# reescritas). Se firma «ad hoc»: basta para usarla en este Mac. Para repartirla haría falta
# firmarla con un certificado de Apple y notarizarla; además, el x265 que trae libheif de
# Homebrew es GPL (ver Contents/Resources/Licencias).
set -euo pipefail

# Identificador del bundle: `local.` no es un dominio real, así que no choca con nadie.
BUNDLE_ID="local.renuevo"

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

features=()
for arg in "$@"; do
  case "$arg" in
    --sin-heic) features=(--no-default-features) ;;
    -h | --help)
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Opción desconocida: $arg (usa --help)" >&2
      exit 2
      ;;
  esac
done

if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi

# Todo se monta en una carpeta temporal y la app anterior solo se sustituye al final: si algo
# falla, no queda una app a medias.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Cargo dice dónde deja el binario, así que da igual cómo esté configurada la carpeta target
# (CARGO_TARGET_DIR, .cargo/config.toml, --target…). Bash 3.2 (el de macOS) da error al
# expandir un array vacío con `set -u`: de ahí el `+`.
cargo build --release -p renuevo-app --message-format=json-render-diagnostics \
  ${features[@]+"${features[@]}"} >"$tmp/cargo.json"
exe_src="$(sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' "$tmp/cargo.json" | grep '/renuevo$' | tail -1)"
if [ ! -f "$exe_src" ]; then
  echo "Cargo no ha dicho dónde está el binario renuevo" >&2
  exit 1
fi

app="$(dirname "$exe_src")/Renuevo.app"
stage="$tmp/Renuevo.app"
contents="$stage/Contents"
exe="$contents/MacOS/renuevo"
frameworks="$contents/Frameworks"
licenses="$contents/Resources/Licencias"

mkdir -p "$contents/MacOS" "$contents/Resources" "$frameworks" "$licenses"
cp "$exe_src" "$exe"
# Sin los símbolos locales (solo sirven para depurar): unos 5 MB menos.
strip -x "$exe"
cp THIRD_PARTY_NOTICES.md "$licenses/"

# Ejecuta un comando y solo muestra lo que dice si falla (install_name_tool avisa de que la
# firma deja de valer y codesign de que la sustituye: se firma todo al final).
quiet() {
  local out
  if ! out="$("$@" 2>&1)"; then
    echo "$out" >&2
    return 1
  fi
}
rename() { quiet install_name_tool "$@"; }

# Rutas de carga de un Mach-O (sin la primera línea, que es el propio archivo).
load_paths() { otool -L "$1" | sed 1d | sed -e 's/^[[:space:]]*//' -e 's/ (compatibility version .*)$//'; }
# Sus LC_RPATH.
rpaths() { otool -l "$1" | sed -n 's/^ *path \(.*\) (offset [0-9]*)$/\1/p'; }

# Dónde está de verdad la librería `$1` que pide el archivo original `$2`, como la buscaría
# dyld. Homebrew usa rutas absolutas, pero algunas fórmulas se enlazan entre sí con @rpath.
resolve() {
  local dep="$1" dir rp
  dir="$(dirname "$2")"
  case "$dep" in
    @loader_path/*) [ -f "$dir/${dep#@loader_path/}" ] && echo "$dir/${dep#@loader_path/}" ;;
    @rpath/*)
      while IFS= read -r rp; do
        case "$rp" in
          @loader_path) rp="$dir" ;;
          @loader_path/*) rp="$dir/${rp#@loader_path/}" ;;
          @* | "") continue ;;
        esac
        if [ -f "$rp/${dep#@rpath/}" ]; then
          echo "$rp/${dep#@rpath/}"
          return
        fi
      done <<<"$(rpaths "$2")"
      ;;
    @*) ;;
    *) [ -f "$dep" ] && echo "$dep" ;;
  esac
  return 0
}

# --- Librerías: copiar las que no son del sistema y apuntar a ellas con @rpath ---
queue=("$exe")        # copias dentro de la app
origins=("$exe_src")  # de dónde salió cada una
i=0
while [ "$i" -lt "${#queue[@]}" ]; do
  file="${queue[$i]}"
  origin="${origins[$i]}"
  i=$((i + 1))
  own_id="$(otool -D "$file" | sed -n 2p)"
  deps="$(load_paths "$file")"
  # Sus LC_RPATH apuntan a carpetas de fuera (las de Homebrew, o las que tuviera el
  # ejecutable): dentro de la app basta uno, el del ejecutable, que se añade al final.
  while IFS= read -r rp; do
    if [ -n "$rp" ]; then rename -delete_rpath "$rp" "$file"; fi
  done <<<"$(rpaths "$file")"
  while IFS= read -r dep <&3; do
    case "$dep" in
      "" | "$own_id" | /usr/lib/* | /System/*) continue ;;
    esac
    src="$(resolve "$dep" "$origin")"
    if [ -z "$src" ]; then
      echo "No encuentro $dep, que pide $origin" >&2
      exit 1
    fi
    real="$(realpath "$src")"
    name="$(basename "$dep")"
    copy="$frameworks/$name"
    if [ -e "$copy" ]; then
      # Ya copiada: tiene que ser la misma, no otra que se llame igual.
      j=0
      while [ "${queue[$j]}" != "$copy" ]; do j=$((j + 1)); done
      if [ "${origins[$j]}" != "$real" ]; then
        echo "Dos librerías distintas se llaman $name: ${origins[$j]} y $real" >&2
        exit 1
      fi
    else
      cp "$real" "$copy"
      chmod u+w "$copy"
      rename -id "@rpath/$name" "$copy"
      queue+=("$copy")
      origins+=("$real")
      # Licencia de la fórmula de Homebrew (…/Cellar/<fórmula>/<versión>/lib/…).
      keg="$(echo "$real" | sed -n 's#^\(.*/Cellar/[^/]*/[^/]*\)/.*#\1#p')"
      if [ -n "$keg" ]; then
        formula="$(basename "$(dirname "$keg")") $(basename "$keg")"
        mkdir -p "$licenses/$formula"
        find "$keg" -maxdepth 1 \( -name 'COPYING*' -o -name 'LICENSE*' \) \
          -exec cp {} "$licenses/$formula/" \;
      fi
    fi
    if [ "$dep" != "@rpath/$name" ]; then
      rename -change "$dep" "@rpath/$name" "$file"
    fi
  done 3<<<"$deps"
done

libs=("${queue[@]:1}")
if [ "${#libs[@]}" -gt 0 ]; then
  rename -add_rpath "@executable_path/../Frameworks" "$exe"
else
  rmdir "$frameworks"
fi

# Comprobación final: nada fuera de la app salvo el sistema, y un único LC_RPATH, el del
# ejecutable, que apunta a Contents/Frameworks.
problems=""
for f in "$exe" ${libs[@]+"${libs[@]}"}; do
  while IFS= read -r dep; do
    case "$dep" in
      "" | /usr/lib/* | /System/*) ;;
      @rpath/*) [ -f "$frameworks/${dep#@rpath/}" ] || problems="$problems$f → $dep (no está)"$'\n' ;;
      *) problems="$problems$f → $dep"$'\n' ;;
    esac
  done <<<"$(load_paths "$f")"
  expected=""
  if [ "$f" = "$exe" ] && [ "${#libs[@]}" -gt 0 ]; then expected="@executable_path/../Frameworks"; fi
  if [ "$(rpaths "$f")" != "$expected" ]; then problems="$problems$f: LC_RPATH $(rpaths "$f" | tr '\n' ' ')"$'\n'; fi
done
if [ -n "$problems" ]; then
  printf 'Quedan rutas fuera de la app:\n%s' "$problems" >&2
  exit 1
fi

# --- Icono, a partir del PNG de 1024 px ---
# Solo 256 y 512 (con sus @2x): si el .icns trae también 16, 32 y 128, macOS 27 los usa en
# pantallas sin Retina y los mete en un recuadro gris; así escala él los tamaños pequeños.
iconset="$tmp/Renuevo.iconset"
mkdir -p "$iconset"
for size in 256 512; do
  quiet sips -z "$size" "$size" crates/renuevo-app/assets/icon.png \
    --out "$iconset/icon_${size}x${size}.png"
  quiet sips -z $((size * 2)) $((size * 2)) crates/renuevo-app/assets/icon.png \
    --out "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$contents/Resources/Renuevo.icns"

# --- Info.plist ---
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
# La versión de macOS más alta que exige alguno de los binarios (las librerías de Homebrew se
# compilan para la versión del sistema en el que se instalaron).
minos() {
  otool -l "$1" | awk '$1 == "cmd" {c = $2}
    v == "" && ((c == "LC_BUILD_VERSION" && $1 == "minos") ||
                (c == "LC_VERSION_MIN_MACOSX" && $1 == "version")) {v = $2}
    END {print v}'
}
for f in "$exe" ${libs[@]+"${libs[@]}"}; do
  v="$(minos "$f")"
  if [ -z "$v" ]; then
    echo "No sé qué versión de macOS pide $f" >&2
    exit 1
  fi
  echo "$v" >>"$tmp/minos"
done
min_macos="$(sort -t. -k1,1n -k2,2n -k3,3n "$tmp/minos" | tail -1)"

# libheif busca plugins (más códecs) en la carpeta de Homebrew en la que se compiló. Si los hay
# (`brew install libheif-plugins`), la app cargaría código de fuera de ella y una segunda
# libheif. LIBHEIF_PLUGIN_PATH la manda a una carpeta vacía del sistema. macOS lo aplica al
# abrir la app desde el Finder, el Dock u `open`, no al ejecutar Contents/MacOS/renuevo a mano.
environment=""
if ls "$frameworks"/libheif* >/dev/null 2>&1; then
  environment="<key>LSEnvironment</key>
	<dict>
		<key>LIBHEIF_PLUGIN_PATH</key>
		<string>/var/empty</string>
	</dict>"
fi

cat >"$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>es</string>
	<key>CFBundleDisplayName</key>
	<string>Renuevo</string>
	<key>CFBundleExecutable</key>
	<string>renuevo</string>
	<key>CFBundleIconFile</key>
	<string>Renuevo</string>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Renuevo</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.photography</string>
	$environment
	<key>LSMinimumSystemVersion</key>
	<string>$min_macos</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST
plutil -lint -s "$contents/Info.plist"

# --- Firma ad hoc: primero lo de dentro, después la app ---
for lib in ${libs[@]+"${libs[@]}"}; do
  quiet codesign --force --sign - "$lib"
done
quiet codesign --force --sign - "$stage"
codesign --verify --strict "$stage"

rm -rf "$app"
mv "$stage" "$app"

echo "Creada: $app"
echo "  macOS $min_macos o posterior · $(du -sh "$app" | cut -f1) · librerías incluidas: ${#libs[@]}"
echo "  Para instalarla (sustituyendo la anterior, si la hay):"
echo "    rm -rf /Applications/Renuevo.app && cp -R \"$app\" /Applications/"
