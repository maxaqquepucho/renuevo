// Pone a una captura de la app el marco de una ventana de macOS (esquinas redondeadas, sombra
// y los tres botones) sobre un degradado con los colores del icono, y la guarda en JPEG.
//
//   swift scripts/frame-screenshots.swift entrada.png salida.jpg [opciones]
//
//   --width N         ancho final en píxeles (por omisión, el de la captura con su margen)
//   --scale N         píxeles por punto de la captura (por omisión 2)
//   --crop x,y,w,h    recorta la captura antes (en píxeles); sin botones de ventana
//   --light           borde para capturas en tema claro
//
// Las capturas salen del test `readme_screenshots` (crates/renuevo-app/tests/render.rs).
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

var args = Array(CommandLine.arguments.dropFirst())
guard args.count >= 2 else { fail("uso: frame-screenshots.swift entrada.png salida.jpg [opciones]") }
let input = URL(fileURLWithPath: args.removeFirst())
let output = URL(fileURLWithPath: args.removeFirst())
var outWidth: CGFloat? = nil
var scale: CGFloat = 2
var crop: CGRect? = nil
var light = false
while !args.isEmpty {
    switch args.removeFirst() {
    case "--width": outWidth = CGFloat(Double(args.removeFirst())!)
    case "--scale": scale = CGFloat(Double(args.removeFirst())!)
    case "--crop":
        let v = args.removeFirst().split(separator: ",").map { CGFloat(Double($0)!) }
        crop = CGRect(x: v[0], y: v[1], width: v[2], height: v[3])
    case "--light": light = true
    case let other: fail("opción desconocida: \(other)")
    }
}

guard let source = CGImageSourceCreateWithURL(input as CFURL, nil),
      var shot = CGImageSourceCreateImageAtIndex(source, 0, nil) else { fail("no se pudo leer \(input.path)") }
if let crop {
    guard let cropped = shot.cropping(to: crop) else { fail("recorte fuera de la imagen") }
    shot = cropped
}
let chrome = crop == nil
let w = CGFloat(shot.width), h = CGFloat(shot.height)
let margin = 64 * scale
let canvas = CGSize(width: w + 2 * margin, height: h + 2 * margin)
let finalWidth = outWidth ?? canvas.width
let k = finalWidth / canvas.width
let finalSize = CGSize(width: finalWidth.rounded(), height: (canvas.height * k).rounded())

let space = CGColorSpace(name: CGColorSpace.sRGB)!
guard let ctx = CGContext(data: nil, width: Int(finalSize.width), height: Int(finalSize.height),
                          bitsPerComponent: 8, bytesPerRow: 0, space: space,
                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { fail("sin contexto") }
ctx.interpolationQuality = .high
ctx.scaleBy(x: k, y: k)

// Fondo: el cielo del icono, de añil a naranja.
let colors = [CGColor(srgbRed: 0.17, green: 0.18, blue: 0.43, alpha: 1),
              CGColor(srgbRed: 0.55, green: 0.36, blue: 0.55, alpha: 1),
              CGColor(srgbRed: 0.96, green: 0.64, blue: 0.36, alpha: 1)] as CFArray
let gradient = CGGradient(colorsSpace: space, colors: colors, locations: [0, 0.55, 1])!
ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: canvas.height), end: CGPoint(x: canvas.width, y: 0), options: [])

// Ventana con sombra (CoreGraphics tiene el origen abajo a la izquierda).
let window = CGRect(x: margin, y: margin, width: w, height: h)
let radius = (chrome ? 14 : 10) * scale
let path = CGPath(roundedRect: window, cornerWidth: radius, cornerHeight: radius, transform: nil)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -14 * scale), blur: 44 * scale,
              color: CGColor(gray: 0, alpha: 0.45))
ctx.addPath(path)
ctx.setFillColor(CGColor(gray: light ? 1 : 0.1, alpha: 1))
ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(path)
ctx.clip()
ctx.draw(shot, in: window)
ctx.restoreGState()

// Botones de la ventana de macOS 27: 14 pt, centrados a 16, 39 y 62 pt del borde y a 16 pt de arriba.
if chrome {
    let lights: [(CGFloat, CGColor)] = [
        (16, CGColor(srgbRed: 1.00, green: 0.37, blue: 0.34, alpha: 1)),
        (39, CGColor(srgbRed: 1.00, green: 0.74, blue: 0.18, alpha: 1)),
        (62, CGColor(srgbRed: 0.16, green: 0.78, blue: 0.25, alpha: 1)),
    ]
    for (x, color) in lights {
        let r = 7 * scale
        let center = CGPoint(x: window.minX + x * scale, y: window.maxY - 16 * scale)
        let dot = CGRect(x: center.x - r, y: center.y - r, width: 2 * r, height: 2 * r)
        ctx.setFillColor(color)
        ctx.fillEllipse(in: dot)
        ctx.setStrokeColor(CGColor(gray: 0, alpha: 0.15))
        ctx.setLineWidth(0.5 * scale)
        ctx.strokeEllipse(in: dot.insetBy(dx: 0.25 * scale, dy: 0.25 * scale))
    }
}

// Borde fino, como el de las ventanas de macOS.
ctx.addPath(path)
ctx.setStrokeColor(CGColor(gray: light ? 0 : 1, alpha: light ? 0.18 : 0.16))
ctx.setLineWidth(1 * scale)
ctx.strokePath()

guard let framed = ctx.makeImage(),
      let dest = CGImageDestinationCreateWithURL(output as CFURL, UTType.jpeg.identifier as CFString, 1, nil)
else { fail("no se pudo crear \(output.path)") }
CGImageDestinationAddImage(dest, framed, [kCGImageDestinationLossyCompressionQuality: 0.86] as CFDictionary)
guard CGImageDestinationFinalize(dest) else { fail("no se pudo escribir \(output.path)") }
print("\(output.lastPathComponent): \(Int(finalSize.width))×\(Int(finalSize.height))")
