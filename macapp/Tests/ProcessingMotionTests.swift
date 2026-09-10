import Foundation

@main
struct ProcessingMotionTests {
    static func main() {
        let start = Date(timeIntervalSinceReferenceDate: 1_000)
        var motion = ProcessingMotion()
        motion.setSpeed(1, duration: 0, at: start)
        let end = start.addingTimeInterval(10)
        let before = motion.value(at: end)
        motion.setSpeed(0, duration: 2.8, at: end)
        assert(abs(motion.value(at: end) - before) < 0.000_001, "Stopping must preserve position")
        let first = motion.value(at: end.addingTimeInterval(0.01)) - before
        let last = motion.value(at: end.addingTimeInterval(2.8)) - motion.value(at: end.addingTimeInterval(2.79))
        assert(first > 0.009 && last < 0.000_01, "Velocity must ease from full speed to rest")
        let settled = motion.value(at: end.addingTimeInterval(2.8))
        assert(abs(motion.value(at: end.addingTimeInterval(100)) - settled) < 0.000_001, "A finished animation must stay still")

        motion.setSpeed(1, duration: 0.7, at: end.addingTimeInterval(101))
        let suspendedAt = end.addingTimeInterval(102)
        let suspended = motion.value(at: suspendedAt)
        motion.setSpeed(0, duration: 0, at: suspendedAt)
        assert(abs(motion.value(at: suspendedAt.addingTimeInterval(60)) - suspended) < 0.000_001, "Background time must not advance the sculpture")
        motion.setSpeed(1, duration: 0.7, at: suspendedAt.addingTimeInterval(60))
        assert(abs(motion.value(at: suspendedAt.addingTimeInterval(60)) - suspended) < 0.000_001, "Resume must preserve orientation")

        for progress in [0.0, 0.5, 1, 3, -1, .nan, .infinity] {
            let phase = ProcessingTransition.clamp(progress)
            assert(phase.isFinite && phase >= 0 && phase <= 1, "Invalid progress must not corrupt the geometry")
        }

        var transition = ProcessingTransition()
        transition.move(to: 0.5, duration: 1, at: start)
        let midpoint = transition.value(at: start.addingTimeInterval(0.5))
        transition.move(to: 1, duration: 2.8, at: start.addingTimeInterval(0.5))
        assert(transition.value(at: start.addingTimeInterval(0.5)) == midpoint, "Completion during an update must not jump")
        assert(transition.value(at: start.addingTimeInterval(3.3)) == 1)
        print("Passed: graceful settling, stable rest, background suspension, continuous resume, valid geometry inputs, interrupted progress updates.")
    }
}
