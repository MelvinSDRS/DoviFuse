import SwiftUI

/// The portfolio's flowing ribbons, adapted to the app's three modes.
/// Geometry follows real progress; decorative motion is never a time estimate.
struct OrganicProcessingField: View {
    let mode: ConversionMode
    let progress: Double
    let active: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    @State private var motion = ProcessingMotion()
    @State private var progressTransition = ProcessingTransition()
    @State private var settling = false
    @State private var pointer = CGPoint.zero

    private var playback: Playback {
        Playback(active: active, visible: scenePhase == .active, reduced: reduceMotion)
    }

    var body: some View {
        GeometryReader { geometry in
            TimelineView(.animation(minimumInterval: 1.0 / 30,
                                    paused: (!active && !settling) || reduceMotion || scenePhase != .active)) { timeline in
                // Resolve timeline-dependent values in the TimelineView
                // content closure. Canvas retains its renderer closure, so a
                // date read only inside that closure can leave the display
                // list stuck until an unrelated hover/layout event redraws it.
                let time = reduceMotion ? 0 : motion.value(at: timeline.date)
                let fraction = reduceMotion ? ProcessingTransition.clamp(progress) : progressTransition.value(at: timeline.date)
                Canvas(opaque: false) { context, size in
                    ProcessingRibbons(size: size, time: time, progress: fraction,
                                  pointer: reduceMotion ? .zero : pointer, mode: mode)
                        .draw(context: &context)
                }
            }
            .onContinuousHover { phase in
                guard !reduceMotion else { return }
                switch phase {
                case .active(let location):
                    // The ribbons react gently without changing the operation.
                    pointer = CGPoint(x: location.x / max(1, geometry.size.width) * 2 - 1,
                                      y: location.y / max(1, geometry.size.height) * 2 - 1)
                case .ended: pointer = .zero
                }
            }
        }
        .onChange(of: progress, initial: true) { _, value in
            progressTransition.move(to: ProcessingTransition.clamp(value), duration: active ? 0.9 : 2.8, at: .now)
        }
        .task(id: playback) {
            guard playback.visible && !playback.reduced else {
                motion.setSpeed(0, duration: 0, at: .now)
                settling = false
                return
            }
            motion.setSpeed(active ? 1 : 0, duration: active ? 0.7 : 2.8, at: .now)
            settling = !active
            if !active {
                do { try await Task.sleep(for: .seconds(2.8)) }
                catch { return }
                settling = false
            }
        }
        .accessibilityHidden(true)
    }

    private struct Playback: Equatable {
        let active: Bool
        let visible: Bool
        let reduced: Bool
    }
}
