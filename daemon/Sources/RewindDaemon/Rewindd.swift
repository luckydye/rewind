import Foundation

@main
struct Rewindd {
    static func main() async {
        do {
            switch try Options.parse(CommandLine.arguments) {
            case .list:
                try await Commands.list()
            case .doctor:
                try await Commands.doctor()
            case .run(let options):
                try await Commands.run(options)
            }
        } catch let error as OptionsError {
            FileHandle.standardError.write(Data("rewindd: \(error)\n\n\(Options.usage)\n".utf8))
            exit(2)
        } catch {
            FileHandle.standardError.write(Data("rewindd: \(error)\n".utf8))
            exit(1)
        }
    }
}
