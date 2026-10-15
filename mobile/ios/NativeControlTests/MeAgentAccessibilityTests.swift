import Flutter
import UIKit

@main
struct MeAgentAccessibilityTests {
  @MainActor static func main() {
    for locked in [false, true] {
      for empty in [false, true] {
        let controller = NativeProfileTextEditorViewController(
          title: "New chat", initialValue: "Chat", placeholder: "Chat name",
          multiline: false, pageBackgroundColor: .systemGroupedBackground,
          containerBackgroundColor: .secondarySystemGroupedBackground,
          containerCornerRadius: 16, allowUnchangedSubmission: false,
          channelDescription: nil, originalName: nil, originalDescription: nil,
          canEditDetails: true, canEditCanvas: false, canvasContent: "",
          canvasLoaded: false,
          chatOptions: ["fieldsLocked": locked, "assistants": empty ? [] : [["id": "agent", "name": "Agent"]]],
          onCancel: {}, onSet: { _ in }
        )
        controller.loadViewIfNeeded()
        let cell = controller.tableView(controller.tableView, cellForRowAt: IndexPath(row: 0, section: 1))
        let disabled = locked || empty
        precondition(cell.accessibilityTraits.contains(.notEnabled) == disabled)
        precondition(cell.accessibilityTraits.contains(.button) == !disabled)
        precondition((cell.selectionStyle == .none) == disabled)
      }
    }
    print("Me agent row accessibility passed")
  }
}
