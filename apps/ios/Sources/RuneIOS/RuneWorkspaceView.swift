import SwiftUI
import Foundation

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
    private static let maximumPersistedTabs = 32
    private static let maximumTitleCharacters = 64

    @State private var tabs: [RuneWorkspaceTab]
    @State private var selectedTabID: UUID
    private let rootURL: URL?

    public init(rootURL: URL? = nil) {
        let firstID = UUID()
        self.rootURL = rootURL
        let restoredTabs = Self.restoreTabs(rootURL: rootURL, fallbackID: firstID)
        _tabs = State(initialValue: restoredTabs)
        _selectedTabID = State(initialValue: restoredTabs[0].id)
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
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(.ultraThinMaterial)
        .onChange(of: tabs) { _, _ in
            persistTabs()
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
        UserDefaults.standard.set(data, forKey: Self.persistedTabsKey)
    }

    private static func restoreTabs(rootURL: URL?, fallbackID: UUID) -> [RuneWorkspaceTab] {
        guard let data = UserDefaults.standard.data(forKey: persistedTabsKey),
              let stored = try? JSONDecoder().decode([RuneStoredTab].self, from: data) else {
            return [RuneWorkspaceTab(id: fallbackID, title: "Main", rootURL: rootURL, sessionID: nil)]
        }
        let valid = stored.prefix(maximumPersistedTabs).filter { tab in
            !tab.title.isEmpty
                && tab.title.count <= maximumTitleCharacters
                && tab.sessionID.map(isValidSessionID) ?? true
        }
        guard !valid.isEmpty else {
            return [RuneWorkspaceTab(id: fallbackID, title: "Main", rootURL: rootURL, sessionID: nil)]
        }
        return valid.map {
            RuneWorkspaceTab(id: $0.id, title: $0.title, rootURL: rootURL, sessionID: $0.sessionID)
        }
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
