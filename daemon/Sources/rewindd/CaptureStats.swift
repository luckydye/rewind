import Foundation

/// Counters written from the capture queue and read from the actor, so they
/// carry their own lock instead of relying on queue affinity.
final class CaptureStats {
    struct Counters {
        var captured = 0
        var encoded = 0
        var dropped = 0
        var idle = 0
        var blank = 0
        var errors = 0
    }

    private let lock = NSLock()
    private var counters = Counters()

    func recordCaptured() { mutate(\.captured) }
    func recordEncoded() { mutate(\.encoded) }
    func recordDropped() { mutate(\.dropped) }
    func recordIdle() { mutate(\.idle) }
    func recordBlank() { mutate(\.blank) }
    func recordError() { mutate(\.errors) }

    func snapshot() -> Counters {
        lock.lock()
        defer { lock.unlock() }
        return counters
    }

    private func mutate(_ keyPath: WritableKeyPath<Counters, Int>) {
        lock.lock()
        counters[keyPath: keyPath] += 1
        lock.unlock()
    }
}
