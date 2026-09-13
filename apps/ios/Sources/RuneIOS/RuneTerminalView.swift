import SwiftUI
import Combine

public struct RuneTranscriptEntry: Identifiable, Hashable, Sendable {
    public enum Kind: Hashable, Sendable {
        case command
        case stdout
        case stderr
        case status
    }

    public let id = UUID()
    public let kind: Kind
    public let text: String

    public init(kind: Kind, text: String) {
        self.kind = kind
        self.text = text
    }
}

@MainActor
public final class RuneTerminalModel: ObservableObject {
    @Published public private(set) var entries: [RuneTranscriptEntry] = []
    @Published public var command = ""
    @Published public private(set) var currentDirectory = "~"
    @Published public private(set) var initializationError: String?

    private let session: RuneFFISession?
    private var history: [String] = []
    private var commandNames: [String] = []
    private var historyCursor: Int?

    public init(rootURL: URL? = nil) {
        let root = rootURL ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        guard let root else {
            session = nil
            initializationError = "Rune could not locate the app Documents directory."
            return
        }
        do {
            session = try RuneFFISession(rootURL: root)
            currentDirectory = session?.currentDirectory ?? "~"
            history = session?.history() ?? []
            commandNames = session?.commands() ?? []
            if let startup = session?.takeStartupOutput() {
                if !startup.stdout.isEmpty {
                    entries.append(.init(kind: .stdout, text: startup.stdout))
                }
                if !startup.stderr.isEmpty {
                    entries.append(.init(kind: .stderr, text: startup.stderr))
                }
                if startup.status != 0 {
                    entries.append(.init(kind: .status, text: "[profile exit \(startup.status)]"))
                }
            }
        } catch {
            session = nil
            initializationError = error.localizedDescription
        }
    }

    public func submit() {
        let line = command.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !line.isEmpty else { return }
        entries.append(.init(kind: .command, text: "\(currentDirectory) $ \(line)"))
        command = ""
        historyCursor = nil
        guard let session else {
            entries.append(.init(kind: .stderr, text: initializationError ?? "Rune session unavailable."))
            return
        }
        let result = session.execute(line)
        if !result.stdout.isEmpty {
            entries.append(.init(kind: .stdout, text: result.stdout))
        }
        if !result.stderr.isEmpty {
            entries.append(.init(kind: .stderr, text: result.stderr))
        }
        if result.status != 0 {
            entries.append(.init(kind: .status, text: "[exit \(result.status)]"))
        }
        currentDirectory = session.currentDirectory
        history = session.history()
    }

    public func previousHistory() {
        guard !history.isEmpty else { return }
        let current = historyCursor ?? history.count
        historyCursor = max(current - 1, 0)
        command = history[historyCursor ?? 0]
    }

    public func nextHistory() {
        guard let current = historyCursor else { return }
        let next = current + 1
        if next >= history.count {
            historyCursor = nil
            command = ""
        } else {
            historyCursor = next
            command = history[next]
        }
    }

    public var completionCandidates: [String] {
        let trimmed = command.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              !trimmed.contains(where: { character in
                  character.isWhitespace || "|;&<>".contains(character)
              })
        else {
            return []
        }
        return commandNames
            .filter { $0.hasPrefix(trimmed) && $0 != trimmed }
            .prefix(8)
            .map { $0 }
    }

    public func applyCompletion(_ candidate: String) {
        let leadingWhitespace = String(command.prefix(while: { $0.isWhitespace }))
        command = "\(leadingWhitespace)\(candidate) "
    }
}

public struct RuneTerminalView: View {
    @StateObject private var model: RuneTerminalModel
    @FocusState private var inputFocused: Bool

    public init(rootURL: URL? = nil) {
        _model = StateObject(wrappedValue: RuneTerminalModel(rootURL: rootURL))
    }

    public var body: some View {
        ZStack {
            RuneBackground()

            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    Circle()
                        .fill(RunePalette.cyan)
                        .frame(width: 8, height: 8)
                        .shadow(color: RunePalette.cyan.opacity(0.8), radius: 8)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("RUNE")
                            .font(.system(size: 12, weight: .bold, design: .rounded))
                            .tracking(2)
                            .foregroundStyle(RunePalette.cyan)
                        Text("RUST CORE / LOCAL SESSION")
                            .font(.system(size: 10, weight: .medium, design: .monospaced))
                            .foregroundStyle(RunePalette.muted)
                    }
                    Spacer()
                    VStack(alignment: .trailing, spacing: 2) {
                        Text(model.currentDirectory)
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundStyle(RunePalette.foreground)
                            .lineLimit(1)
                        Text("\(model.entries.count) events")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(RunePalette.muted)
                    }
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 14)
                .background(RunePalette.panel.opacity(0.94))
                .overlay(alignment: .bottom) {
                    Rectangle()
                        .fill(RunePalette.cyan.opacity(0.22))
                        .frame(height: 1)
                }

                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 9) {
                            if let initializationError = model.initializationError {
                                Text(initializationError)
                                    .foregroundStyle(RunePalette.error)
                                    .textSelection(.enabled)
                            }
                            ForEach(model.entries) { entry in
                                Text(entry.text)
                                    .font(.system(size: 15, design: .monospaced))
                                    .foregroundStyle(color(for: entry.kind))
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .textSelection(.enabled)
                                    .id(entry.id)
                            }
                        }
                        .padding(18)
                    }
                    .scrollDismissesKeyboard(.interactively)
                    .onChange(of: model.entries.count) { _, _ in
                        if let last = model.entries.last {
                            withAnimation(.easeOut(duration: 0.15)) {
                                proxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                }

                if !model.completionCandidates.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(model.completionCandidates, id: \.self) { candidate in
                                Button(candidate) {
                                    model.applyCompletion(candidate)
                                    inputFocused = true
                                }
                                .buttonStyle(.plain)
                                .font(.system(size: 13, weight: .medium, design: .monospaced))
                                .foregroundStyle(RunePalette.foreground)
                                .padding(.horizontal, 11)
                                .padding(.vertical, 8)
                                .background(RunePalette.cyan.opacity(0.12))
                                .overlay {
                                    Capsule()
                                        .stroke(RunePalette.cyan.opacity(0.45), lineWidth: 1)
                                }
                                .clipShape(Capsule())
                                .accessibilityLabel("Complete with \(candidate)")
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 7)
                    }
                }

                HStack(alignment: .bottom, spacing: 10) {
                    Text("\(model.currentDirectory) ›")
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundStyle(RunePalette.cyan)
                        .lineLimit(1)
                    TextField("Enter a Rune command", text: $model.command, axis: .vertical)
                        .font(.system(size: 15, design: .monospaced))
                        .foregroundStyle(RunePalette.foreground)
                        .textFieldStyle(.plain)
                        .lineLimit(1...4)
                        .focused($inputFocused)
                        .autocorrectionDisabled(true)
                        .textInputAutocapitalization(.never)
                        .onSubmit {
                            model.submit()
                            inputFocused = true
                        }
                        .accessibilityLabel("Command input")
                    Button {
                        model.previousHistory()
                        inputFocused = true
                    } label: {
                        Image(systemName: "chevron.up")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(RunePalette.muted)
                    .accessibilityLabel("Previous command")
                    Button {
                        model.nextHistory()
                        inputFocused = true
                    } label: {
                        Image(systemName: "chevron.down")
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(RunePalette.muted)
                    .accessibilityLabel("Next command")
                    Button {
                        model.submit()
                        inputFocused = true
                    } label: {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.title2)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(RunePalette.cyan)
                    .accessibilityLabel("Execute command")
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(RunePalette.panel.opacity(0.97))
                .overlay(alignment: .top) {
                    Rectangle()
                        .fill(RunePalette.ember.opacity(0.5))
                        .frame(height: 1)
                }
            }
        }
        .preferredColorScheme(.dark)
        .onAppear { inputFocused = true }
    }

    private func color(for kind: RuneTranscriptEntry.Kind) -> Color {
        switch kind {
        case .command: return RunePalette.cyan
        case .stdout: return RunePalette.foreground
        case .stderr: return RunePalette.ember
        case .status: return RunePalette.muted
        }
    }
}

private enum RunePalette {
    static let background = Color(red: 0.018, green: 0.024, blue: 0.035)
    static let panel = Color(red: 0.045, green: 0.058, blue: 0.078)
    static let foreground = Color(red: 0.88, green: 0.93, blue: 0.95)
    static let muted = Color(red: 0.43, green: 0.52, blue: 0.58)
    static let cyan = Color(red: 0.18, green: 0.88, blue: 0.93)
    static let ember = Color(red: 1.0, green: 0.54, blue: 0.25)
    static let error = Color(red: 1.0, green: 0.34, blue: 0.4)
}

private struct RuneBackground: View {
    var body: some View {
        ZStack {
            RunePalette.background
            RadialGradient(
                colors: [RunePalette.cyan.opacity(0.13), .clear],
                center: .topLeading,
                startRadius: 0,
                endRadius: 500
            )
            RadialGradient(
                colors: [RunePalette.ember.opacity(0.08), .clear],
                center: .bottomTrailing,
                startRadius: 0,
                endRadius: 420
            )
        }
        .ignoresSafeArea()
    }
}

public struct RuneIOSApp: App {
    public init() {}

    public var body: some Scene {
        WindowGroup {
            RuneTerminalView()
        }
    }
}
