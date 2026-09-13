import SwiftUI

private enum RuneANSIColor {
    case indexed(Int)
    case rgb(Int, Int, Int)

    func resolve() -> Color {
        switch self {
        case .indexed(let index):
            return Self.palette[index.clamped(to: 0...255)]
        case .rgb(let red, let green, let blue):
            return Color(
                red: Double(red.clamped(to: 0...255)) / 255,
                green: Double(green.clamped(to: 0...255)) / 255,
                blue: Double(blue.clamped(to: 0...255)) / 255
            )
        }
    }

    private static let palette: [Color] = {
        let base = [
            (0, 0, 0),
            (205, 49, 49),
            (13, 188, 121),
            (229, 229, 16),
            (36, 114, 200),
            (188, 63, 188),
            (17, 168, 205),
            (229, 229, 229),
        ]
        let bright = [
            (102, 102, 102),
            (241, 76, 76),
            (35, 209, 139),
            (245, 245, 67),
            (59, 142, 234),
            (214, 112, 214),
            (41, 184, 219),
            (255, 255, 255),
        ]
        var colors = (base + bright).map { red, green, blue in
            Color(
                red: Double(red) / 255,
                green: Double(green) / 255,
                blue: Double(blue) / 255
            )
        }
        for red in 0..<6 {
            for green in 0..<6 {
                for blue in 0..<6 {
                    let index = 16 + (red * 36) + (green * 6) + blue
                    colors.append(Color(
                        red: Double(red == 0 ? 0 : 55 + red * 40) / 255,
                        green: Double(green == 0 ? 0 : 55 + green * 40) / 255,
                        blue: Double(blue == 0 ? 0 : 55 + blue * 40) / 255
                    ))
                    assert(colors.count == index + 1)
                }
            }
        }
        for gray in 0..<24 {
            let value = Double(8 + gray * 10) / 255
            colors.append(Color(red: value, green: value, blue: value))
        }
        return colors
    }()
}

private struct RuneANSIStyle {
    var foreground: RuneANSIColor?
    var background: RuneANSIColor?
    var bold = false
    var underline = false
    var inverse = false
}

private struct RuneANSISegment {
    let text: String
    let style: RuneANSIStyle
}

/// Displays terminal text after consuming common ANSI control sequences.
/// Unsupported controls are removed rather than shown as escape bytes.
struct RuneANSIText: View {
    let text: String
    let defaultColor: Color
    let defaultBackground: Color

    init(text: String, defaultColor: Color, defaultBackground: Color = .clear) {
        self.text = text
        self.defaultColor = defaultColor
        self.defaultBackground = defaultBackground
    }

    var body: some View {
        styledText
    }

    private var styledText: Text {
        RuneANSIRenderer.segments(from: text).reduce(Text("")) { result, segment in
            let regularForeground = segment.style.foreground?.resolve() ?? defaultColor
            let regularBackground = segment.style.background?.resolve()
            let foregroundColor = segment.style.inverse
                ? regularBackground ?? defaultBackground
                : regularForeground
            let backgroundColor = segment.style.inverse
                ? regularForeground
                : regularBackground
            var fragment = Text(segment.text)
                .foregroundColor(foregroundColor)
            if segment.style.bold {
                fragment = fragment.bold()
            }
            if segment.style.underline {
                fragment = fragment.underline()
            }
            if let backgroundColor {
                fragment = fragment.background(backgroundColor)
            }
            return result + fragment
        }
    }
}

private enum RuneANSIRenderer {
    static func segments(from text: String) -> [RuneANSISegment] {
        var segments = [RuneANSISegment]()
        var style = RuneANSIStyle(foreground: nil, background: nil)
        var pending = ""
        let scalars = text.unicodeScalars
        var index = scalars.startIndex

        func flush() {
            guard !pending.isEmpty else { return }
            segments.append(RuneANSISegment(text: pending, style: style))
            pending.removeAll(keepingCapacity: true)
        }

        while index < scalars.endIndex {
            let scalar = scalars[index]
            guard scalar.value == 0x1b else {
                pending.unicodeScalars.append(scalar)
                index = scalars.index(after: index)
                continue
            }

            flush()
            let controlStart = scalars.index(after: index)
            guard controlStart < scalars.endIndex else { break }
            switch scalars[controlStart].value {
            case 0x5b: // CSI: ESC [ ... final
                let parameterStart = scalars.index(after: controlStart)
                var cursor = parameterStart
                while cursor < scalars.endIndex, !isCSIFinal(scalars[cursor].value) {
                    cursor = scalars.index(after: cursor)
                }
                guard cursor < scalars.endIndex else {
                    index = cursor
                    continue
                }
                if scalars[cursor].value == 0x6d { // SGR
                    let parameters = String(scalars[parameterStart..<cursor])
                    applySGR(parameters, to: &style)
                }
                index = scalars.index(after: cursor)
            case 0x5d: // OSC: consume until BEL or ST
                index = consumeOSC(scalars, after: controlStart)
            default:
                index = scalars.index(after: controlStart)
            }
        }
        flush()
        return segments
    }

    private static func isCSIFinal(_ value: UInt32) -> Bool {
        (0x40...0x7e).contains(value)
    }

    private static func consumeOSC(
        _ scalars: String.UnicodeScalarView,
        after controlStart: String.UnicodeScalarView.Index
    ) -> String.UnicodeScalarView.Index {
        var cursor = scalars.index(after: controlStart)
        while cursor < scalars.endIndex {
            let value = scalars[cursor].value
            if value == 0x07 { // BEL
                return scalars.index(after: cursor)
            }
            if value == 0x1b {
                let next = scalars.index(after: cursor)
                if next < scalars.endIndex, scalars[next].value == 0x5c { // ST
                    return scalars.index(after: next)
                }
            }
            cursor = scalars.index(after: cursor)
        }
        return cursor
    }

    private static func applySGR(_ parameters: String, to style: inout RuneANSIStyle) {
        let values = parameters.isEmpty
            ? [0]
            : parameters.split(separator: ";", omittingEmptySubsequences: false).map {
                Int($0) ?? 0
            }
        var index = 0
        while index < values.count {
            let value = values[index]
            switch value {
            case 0:
                style = RuneANSIStyle(foreground: nil, background: nil)
            case 1:
                style.bold = true
            case 22:
                style.bold = false
            case 4:
                style.underline = true
            case 24:
                style.underline = false
            case 7:
                style.inverse = true
            case 27:
                style.inverse = false
            case 30...37:
                style.foreground = .indexed(value - 30)
            case 39:
                style.foreground = nil
            case 40...47:
                style.background = .indexed(value - 40)
            case 49:
                style.background = nil
            case 90...97:
                style.foreground = .indexed(value - 90 + 8)
            case 100...107:
                style.background = .indexed(value - 100 + 8)
            case 38:
                if let next = extendedColor(values, after: index) {
                    style.foreground = next.color
                    index = next.nextIndex
                }
            case 48:
                if let next = extendedColor(values, after: index) {
                    style.background = next.color
                    index = next.nextIndex
                }
            default:
                break
            }
            index += 1
        }
    }

    private static func extendedColor(
        _ values: [Int],
        after index: Int
    ) -> (color: RuneANSIColor, nextIndex: Int)? {
        guard let mode = values[safe: index + 1] else { return nil }
        switch mode {
        case 5:
            guard let value = values[safe: index + 2] else { return nil }
            return (.indexed(value), index + 2)
        case 2:
            guard let red = values[safe: index + 2],
                  let green = values[safe: index + 3],
                  let blue = values[safe: index + 4] else { return nil }
            return (.rgb(red, green, blue), index + 4)
        default:
            return nil
        }
    }
}

private extension Array {
    subscript(safe index: Index) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

private extension Int {
    func clamped(to range: ClosedRange<Int>) -> Int {
        Swift.min(Swift.max(self, range.lowerBound), range.upperBound)
    }
}
