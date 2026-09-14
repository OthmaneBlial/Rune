import SwiftUI
import Foundation

/// Value routed by SwiftUI when a second Rune window is opened.
public struct RuneWindowRoute: Codable, Hashable, Sendable {
    public let sessionID: String

    public init(sessionID: String) {
        self.sessionID = sessionID
    }
}

private struct RuneWorkspaceTab: Identifiable, Hashable {
    let id: UUID
    let title: String
    let rootURL: URL?
    let sessionID: String?
}

private struct RuneStoredTab: Codable {
    let id: UUID
    let title: String
    let sessionID: String?
}

/// Source-only tab container. Each tab creates its own Rust session namespace;
/// the terminal view remains responsible for command rendering and input.
public struct RuneWorkspaceView: View {
    private static let persistedTabsKey = "rune.workspace-tabs.v1"
    private static let windowTabsKeyPrefix = "rune.workspace-tabs.window."
    private static let maximumPersistedTabs = 32
    private static let maximumTitleCharacters = 64

    @Environment(\.openWindow) private var openWindow
    @Environment(\.scenePhase) private var scenePhase
    @State private var tabs: [RuneWorkspaceTab]
    @State private var selectedTabID: UUID
    private let rootURL: URL?
    private let storageKey: String

    public init(rootURL: URL? = nil) {
        self.init(rootURL: rootURL, sessionID: nil)
    }

    public init(rootURL: URL? = nil, sessionID: String?) {
        let firstID = UUID()
        self.rootURL = rootURL
        self.storageKey = Self.storageKey(for: sessionID)
        let restoredTabs = Self.restoreTabs(
            rootURL: rootURL,
            fallbackID: firstID,
            fallbackSessionID: sessionID,
            storageKey: storageKey
        )
        _tabs = State(initialValue: restoredTabs)
        _selectedTabID = State(
            initialValue: Self.restoreSelectedTabID(
                tabs: restoredTabs,
                storageKey: storageKey
            )
        )
    }

    public var body: some View {
        VStack(spacing: 0) {
            tabBar
            if let selectedTab = tabs.first(where: { $0.id == selectedTabID }) {
                RuneTerminalView(rootURL: selectedTab.rootURL, sessionID: selectedTab.sessionID)
                    .id(selectedTab.id)
            } else {
                Text("Rune session unavailable")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    private var tabBar: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(tabs) { tab in
                        HStack(spacing: 4) {
                            Button(tab.title) {
                                selectedTabID = tab.id
                            }
                            .buttonStyle(.plain)
                            .font(.system(size: 12, design: .monospaced))
                            .lineLimit(1)
                            .accessibilityLabel("Terminal tab \(tab.title)")
                            .accessibilityValue(tab.id == selectedTabID ? "Selected" : "")
                            .accessibilityHint("Switch to this independent Rust session.")
                            .accessibilityAddTraits(tab.id == selectedTabID ? .isSelected : [])
                            if tabs.count > 1 {
                                Button {
                                    close(tab)
                                } label: {
                                    Image(systemName: "xmark")
                                        .font(.system(size: 9, weight: .bold))
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Close \(tab.title)")
                            }
                        }
                        .padding(.horizontal, 10)
                        .padding(.vertical, 7)
                        .background(tab.id == selectedTabID ? Color.accentColor.opacity(0.18) : Color.clear)
                        .clipShape(Capsule())
                    }
                }
            }
            Button {
                addTab()
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 13, weight: .bold))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("New Rune session")
            .accessibilityHint("Open another independent terminal tab.")
            .accessibilityIdentifier("rune.newTab")
            Button {
                openWindow(value: RuneWindowRoute(sessionID: UUID().uuidString.lowercased()))
            } label: {
                Image(systemName: "rectangle.on.rectangle")
                    .font(.system(size: 13, weight: .bold))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Open a new Rune window")
            .accessibilityHint("Open an independent Rune session window.")
            .accessibilityIdentifier("rune.newWindow")
            .keyboardShortcut("n", modifiers: [.command])
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(.ultraThinMaterial)
        .onChange(of: tabs) { _, _ in
            persistTabs()
        }
        .onChange(of: selectedTabID) { _, _ in
            persistSelectedTab()
        }
        .onChange(of: scenePhase) { _, phase in
            guard phase != .active else { return }
            persistTabs()
            persistSelectedTab()
        }
    }

    private func addTab() {
        let index = tabs.count + 1
        let tab = RuneWorkspaceTab(
            id: UUID(),
            title: "Session \(index)",
            rootURL: rootURL,
            sessionID: UUID().uuidString.lowercased()
        )
        tabs.append(tab)
        selectedTabID = tab.id
        persistTabs()
    }

    private func close(_ tab: RuneWorkspaceTab) {
        guard tabs.count > 1, let index = tabs.firstIndex(of: tab) else { return }
        tabs.remove(at: index)
        if selectedTabID == tab.id {
            selectedTabID = tabs[min(index, tabs.count - 1)].id
        }
        persistTabs()
    }

    private func persistTabs() {
        let stored = tabs.prefix(Self.maximumPersistedTabs).map {
            RuneStoredTab(id: $0.id, title: $0.title, sessionID: $0.sessionID)
        }
        guard let data = try? JSONEncoder().encode(stored) else { return }
        UserDefaults.standard.set(data, forKey: storageKey)
        persistSelectedTab()
    }

    private func persistSelectedTab() {
        UserDefaults.standard.set(
            selectedTabID.uuidString,
            forKey: Self.selectedTabStorageKey(for: storageKey)
        )
    }

    private static func restoreTabs(
        rootURL: URL?,
        fallbackID: UUID,
        fallbackSessionID: String?,
        storageKey: String
    ) -> [RuneWorkspaceTab] {
        guard let data = UserDefaults.standard.data(forKey: storageKey),
              let stored = try? JSONDecoder().decode([RuneStoredTab].self, from: data) else {
            return [RuneWorkspaceTab(
                id: fallbackID,
                title: "Main",
                rootURL: rootURL,
                sessionID: fallbackSessionID
            )]
        }
        let valid = stored.prefix(maximumPersistedTabs).filter { tab in
            !tab.title.isEmpty
                && tab.title.count <= maximumTitleCharacters
                && tab.sessionID.map(isValidSessionID) ?? true
        }
        guard !valid.isEmpty else {
            return [RuneWorkspaceTab(
                id: fallbackID,
                title: "Main",
                rootURL: rootURL,
                sessionID: fallbackSessionID
            )]
        }
        return valid.map {
            RuneWorkspaceTab(id: $0.id, title: $0.title, rootURL: rootURL, sessionID: $0.sessionID)
        }
    }

    private static func restoreSelectedTabID(
        tabs: [RuneWorkspaceTab],
        storageKey: String
    ) -> UUID {
        guard let rawID = UserDefaults.standard.string(forKey: selectedTabStorageKey(for: storageKey)),
              let selectedID = UUID(uuidString: rawID),
              tabs.contains(where: { $0.id == selectedID }) else {
            return tabs[0].id
        }
        return selectedID
    }

    private static func selectedTabStorageKey(for storageKey: String) -> String {
        storageKey + ".selected-tab"
    }

    private static func storageKey(for sessionID: String?) -> String {
        guard let sessionID, isValidSessionID(sessionID) else {
            return persistedTabsKey
        }
        return windowTabsKeyPrefix + sessionID
    }

    private static func isValidSessionID(_ value: String) -> Bool {
        !value.isEmpty
            && value.count <= 64
            && value.utf8.allSatisfy {
                ($0 >= 65 && $0 <= 90)
                    || ($0 >= 97 && $0 <= 122)
                    || ($0 >= 48 && $0 <= 57)
                    || $0 == 95
                    || $0 == 45
                    || $0 == 46
            }
    }
}
