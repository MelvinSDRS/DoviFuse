import SwiftUI

struct SettingsView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Form {
            Section("Scratch Storage") {
                LabeledContent("Folder") {
                    Text(model.scratchURL?.path ?? "Not selected")
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Button("Choose Folder…") {
                    model.chooseScratchFolder()
                }
            }

            Section("DV7 Enhancement-Layer Archive") {
                Toggle("Save the original EL + RPU", isOn: $model.saveEnhancementLayer)
                LabeledContent("Folder") {
                    Text(model.archiveURL?.path ?? "Not selected")
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Button("Choose Archive Folder…") { model.chooseArchiveFolder() }
                Text("Standard conversion removes the enhancement layer. Archiving preserves it separately; it does not restore FEL picture detail to the converted movie.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let error = model.archiveError {
                    Text(error).font(.caption).foregroundStyle(.orange)
                }
            }

            Section("Performance") {
                Picker("Hardware decoding", selection: $model.hardware) {
                    ForEach(HardwareMode.allCases) { mode in
                        Text(mode.label).tag(mode)
                    }
                }

                Text("Auto uses VideoToolbox when the selected file supports it and falls back to software decoding when necessary.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .disabled(model.isRunning)
        .formStyle(.grouped)
        .scenePadding()
        .frame(width: 540, height: 600)
    }
}
