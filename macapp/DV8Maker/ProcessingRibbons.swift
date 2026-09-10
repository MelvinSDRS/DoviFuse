import SwiftUI

struct ProcessingRibbonPalette {
    let spectrum: [Color]
    let text: Color
    let core: [Color]
    var primary: Color { spectrum[1] }
    var highlight: Color { spectrum[3] }

    static func forMode(_ mode: ConversionMode) -> Self {
        switch mode {
        case .hybrid: hybrid
        case .standard: standard
        case .checker: checker
        }
    }

    private static let hybrid = Self(
        spectrum: [color(0x5465ff), color(0xad8aff), color(0xf894ff), color(0xffc594)],
        text: color(0xe8d6ff), core: [color(0x46425b), color(0x171320)])
    private static let standard = Self(
        spectrum: [color(0xd47727), color(0xffa65c), color(0xffc582), color(0xffe2ae)],
        text: color(0xffead3), core: [color(0x514034), color(0x241910)])
    private static let checker = Self(
        spectrum: [color(0x289d58), color(0x53cf7b), color(0x92e8a4), color(0xcef5ae)],
        text: color(0xddf7e4), core: [color(0x254633), color(0x102017)])

    private static func color(_ hex: UInt32) -> Color {
        Color(red: Double((hex >> 16) & 255) / 255,
              green: Double((hex >> 8) & 255) / 255,
              blue: Double(hex & 255) / 255)
    }

    static func coreSize(height: Double) -> Double { max(0, min(176, height - 28)) }
}

/// Draws only ribbons, traveling light, and the central object. The rest of the
/// canvas remains transparent: there is no background fill or ambient halo.
struct ProcessingRibbons {
    let size: CGSize
    let time: Double
    let progress: Double
    let pointer: CGPoint
    let mode: ConversionMode

    private var palette: ProcessingRibbonPalette { .forMode(mode) }
    private var center: CGPoint { CGPoint(x: size.width / 2, y: size.height / 2) }
    private var charge: Double { sin(progress * .pi) }
    private var span: Double { min(150, size.height * 0.34) }

    func draw(context: inout GraphicsContext) {
        let ribbons = makeRibbons()
        for (index, ribbon) in ribbons.enumerated() {
            let main = index == ribbons.count / 2
            let opacity = main ? 0.9 : 0.30 + Double(index % 3) * 0.08
            let shading = spectrum(opacity: opacity)
            if main {
                context.stroke(ribbon.path, with: spectrum(opacity: 0.055), lineWidth: 5)
            }
            context.stroke(ribbon.path, with: shading,
                           style: StrokeStyle(lineWidth: main ? 1.8 : 0.8, lineCap: .round))
        }
        drawTravelers(context: &context, ribbons: ribbons)
        if mode == .checker { drawScan(context: &context, ribbons: ribbons) }
        drawCore(context: &context)
    }

    private func spectrum(opacity: Double) -> GraphicsContext.Shading {
        let colors = palette.spectrum
        return .linearGradient(Gradient(stops: [
            .init(color: colors[0].opacity(0), location: 0),
            .init(color: colors[0].opacity(opacity), location: 0.09),
            .init(color: colors[1].opacity(opacity), location: 0.38),
            .init(color: colors[2].opacity(opacity), location: 0.62),
            .init(color: colors[3].opacity(opacity), location: 0.91),
            .init(color: colors[3].opacity(0), location: 1)
        ]), startPoint: CGPoint(x: 0, y: center.y), endPoint: CGPoint(x: size.width, y: center.y))
    }

    private func makeRibbons() -> [Ribbon] {
        let families: [Double] = mode == .hybrid ? [-1, 1] : [0]
        let count = mode == .hybrid ? 8 : 11
        return families.flatMap { family in
            (0..<count).map { index in
                let lane = (Double(index) / Double(count - 1) - 0.5) * 2
                let offset = lane * span
                let wave = sin(time * 0.65 + Double(index) * 0.38 + family) * (5 + charge * 16)
                    + pointer.y * 7 + pointer.x * lane * 5
                let left: Bezier
                let right: Bezier
                let cx = center.x, cy = center.y, w = size.width
                switch mode {
                case .hybrid:
                    // Two separate inputs interleave as one spectrum at the core.
                    let input = family * span * 0.64 + offset * 0.34
                    let output = offset * 0.76 + family * span * (0.24 - progress * 0.18)
                    left = Bezier(CGPoint(x: 0, y: cy + input),
                                  CGPoint(x: w * 0.24, y: cy + input * 0.95 + wave),
                                  CGPoint(x: w * 0.38, y: cy - input * 0.42 - wave), center)
                    right = Bezier(center, CGPoint(x: w * 0.62, y: cy + output * 0.55 + wave),
                                   CGPoint(x: w * 0.77, y: cy - output * 0.85 - wave),
                                   CGPoint(x: w, y: cy - output))
                case .standard:
                    // A folded source ribbon becomes progressively straighter.
                    let fold = 1 - progress * 0.72
                    left = Bezier(CGPoint(x: 0, y: cy + offset),
                                  CGPoint(x: w * 0.24, y: cy + offset * 0.5 + wave),
                                  CGPoint(x: w * 0.35, y: cy - offset * fold - wave), center)
                    right = Bezier(center, CGPoint(x: w * 0.64, y: cy + offset * fold * 0.7 + wave * fold),
                                   CGPoint(x: w * 0.80, y: cy + offset * (0.8 - progress * 0.58)),
                                   CGPoint(x: w, y: cy + offset * (1 - progress * 0.76)))
                case .checker:
                    // Parallel tracks stay stable while the inspection light sweeps.
                    let middle = CGPoint(x: cx, y: cy + offset * 0.16)
                    left = Bezier(CGPoint(x: 0, y: cy + offset),
                                  CGPoint(x: w * 0.25, y: cy + offset + wave * 0.12),
                                  CGPoint(x: w * 0.40, y: cy + offset * 0.16), middle)
                    right = Bezier(middle, CGPoint(x: w * 0.60, y: cy + offset * 0.16),
                                   CGPoint(x: w * 0.75, y: cy + offset - wave * 0.12),
                                   CGPoint(x: w, y: cy + offset))
                }
                return Ribbon(left: left, right: right)
            }
        }
    }

    private func drawTravelers(context: inout GraphicsContext, ribbons: [Ribbon]) {
        let count = mode == .checker ? 28 : 80
        for index in 0..<count {
            let phase = seed(Double(index))
            let travel = (phase + time * (0.08 + charge * 0.035)).truncatingRemainder(dividingBy: 1)
            let ribbon = ribbons[index % ribbons.count]
            let point = ribbon.point(at: travel)
            let tail = ribbon.point(at: max(0, travel - 0.012))
            let edgeFade = ProcessingTransition.smooth(travel / 0.09) * ProcessingTransition.smooth((1 - travel) / 0.09)
            let color = mode == .checker || travel < 0.5 ? palette.text : palette.highlight
            var path = Path()
            path.move(to: tail)
            path.addLine(to: point)
            context.stroke(path, with: .color(color.opacity((0.25 + phase * 0.45) * edgeFade)),
                           style: StrokeStyle(lineWidth: 0.7 + phase * 0.8, lineCap: .round))
        }
    }

    private func drawScan(context: inout GraphicsContext, ribbons: [Ribbon]) {
        let x = size.width * (0.10 + (sin(time * 0.48) + 1) * 0.40)
        let top = CGPoint(x: x, y: center.y - span * 1.15)
        let bottom = CGPoint(x: x, y: center.y + span * 1.15)
        var beam = Path()
        beam.move(to: top)
        beam.addLine(to: bottom)
        context.stroke(beam, with: .linearGradient(
            Gradient(colors: [.clear, palette.highlight.opacity(0.55), .clear]),
            startPoint: top, endPoint: bottom), lineWidth: 1)
        for ribbon in ribbons {
            let point = ribbon.point(atX: x)
            context.stroke(Path(ellipseIn: CGRect(x: point.x - 4, y: point.y - 4, width: 8, height: 8)),
                           with: .color(palette.highlight.opacity(0.20)), lineWidth: 1)
            context.fill(Path(ellipseIn: CGRect(x: point.x - 1.6, y: point.y - 1.6, width: 3.2, height: 3.2)),
                         with: .color(palette.highlight.opacity(0.9)))
        }
    }

    private func drawCore(context: inout GraphicsContext) {
        let side = ProcessingRibbonPalette.coreSize(height: size.height)
        let rect = CGRect(x: center.x - side / 2, y: center.y - side / 2, width: side, height: side)
        let path = Path(roundedRect: rect, cornerRadius: side * 0.26)
        let start = CGPoint(x: rect.minX, y: rect.minY)
        let end = CGPoint(x: rect.maxX, y: rect.maxY)
        // Emissive edges belong to the object; no glow is painted across the canvas.
        context.stroke(path, with: .color(palette.primary.opacity(0.045)), lineWidth: 9)
        context.stroke(path, with: .color(palette.primary.opacity(0.09)), lineWidth: 4)
        context.fill(path, with: .linearGradient(Gradient(colors: palette.core), startPoint: start, endPoint: end))
        context.stroke(path, with: .linearGradient(Gradient(colors: [
            palette.text.opacity(0.70), palette.primary.opacity(0.25),
            palette.highlight.opacity(0.55)
        ]), startPoint: start, endPoint: end), lineWidth: 1.2)
    }

    private func seed(_ value: Double) -> Double {
        let n = sin(value * 127.1 + 31.7) * 43758.5453
        return n - floor(n)
    }

    private struct Bezier {
        let a: CGPoint, b: CGPoint, c: CGPoint, d: CGPoint
        init(_ a: CGPoint, _ b: CGPoint, _ c: CGPoint, _ d: CGPoint) {
            self.a = a; self.b = b; self.c = c; self.d = d
        }
        func point(at t: Double) -> CGPoint {
            let u = 1 - t
            return CGPoint(x: u * u * u * a.x + 3 * u * u * t * b.x + 3 * u * t * t * c.x + t * t * t * d.x,
                           y: u * u * u * a.y + 3 * u * u * t * b.y + 3 * u * t * t * c.y + t * t * t * d.y)
        }
    }

    private struct Ribbon {
        let left: Bezier, right: Bezier
        var path: Path {
            var path = Path()
            path.move(to: left.a)
            path.addCurve(to: left.d, control1: left.b, control2: left.c)
            path.addCurve(to: right.d, control1: right.b, control2: right.c)
            return path
        }
        func point(at t: Double) -> CGPoint { t < 0.5 ? left.point(at: t * 2) : right.point(at: (t - 0.5) * 2) }
        func point(atX x: Double) -> CGPoint {
            var low = 0.0, high = 1.0
            for _ in 0..<14 {
                let middle = (low + high) / 2
                if point(at: middle).x < x { low = middle } else { high = middle }
            }
            return point(at: (low + high) / 2)
        }
    }
}
