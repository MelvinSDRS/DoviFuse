import Foundation

/// Integrating velocity keeps every filament in place when a job ends or the
/// app goes into the background, instead of resetting the animation's time.
struct ProcessingMotion {
    private var origin = Date.now
    private var offset = 0.0
    private var initialSpeed = 0.0
    private var targetSpeed = 0.0
    private var duration = 0.0

    func value(at date: Date) -> Double {
        let elapsed = max(0, date.timeIntervalSince(origin))
        guard duration > 0 else { return offset + elapsed * targetSpeed }
        let u = min(1, elapsed / duration)
        let integratedEase = duration * (u * u * u - 0.5 * u * u * u * u)
        return offset + initialSpeed * min(elapsed, duration)
            + (targetSpeed - initialSpeed) * integratedEase
            + targetSpeed * max(0, elapsed - duration)
    }

    mutating func setSpeed(_ speed: Double, duration: Double, at date: Date) {
        let position = value(at: date)
        let u = self.duration > 0 ? date.timeIntervalSince(origin) / self.duration : 1
        let currentSpeed = initialSpeed + (targetSpeed - initialSpeed) * ProcessingTransition.smooth(u)
        offset = position
        initialSpeed = currentSpeed
        targetSpeed = speed
        self.duration = duration
        origin = date
    }
}

struct ProcessingTransition {
    private var origin = Date.now
    private var start = 0.0
    private(set) var target = 0.0
    private var duration = 0.0

    static func smooth(_ value: Double) -> Double {
        let t = min(1, max(0, value))
        return t * t * (3 - 2 * t)
    }

    static func clamp(_ progress: Double) -> Double {
        min(1, max(0, progress.isFinite ? progress : 0))
    }

    func value(at date: Date) -> Double {
        guard duration > 0 else { return target }
        return start + (target - start) * Self.smooth(date.timeIntervalSince(origin) / duration)
    }

    mutating func move(to target: Double, duration: Double, at date: Date) {
        start = value(at: date)
        self.target = target
        self.duration = duration
        origin = date
    }
}
