// Speaks text with a system voice into a WAV file and writes the time of
// every word as JSON ({"words": [{"word", "start", "end"}]}), ready for a
// ultrafast `captions` node and an `audio` node. macOS 14+.
//
//   swift tools/say_words.swift "Text to speak." vo.wav words.json [Voice] [rate]
import AVFoundation
import Foundation

let args = CommandLine.arguments
guard args.count >= 4 else {
    FileHandle.standardError.write("usage: say_words.swift <text> <out.wav> <out.json> [voice] [rate 0..1]\n".data(using: .utf8)!)
    exit(2)
}
let text = args[1]
let wavURL = URL(fileURLWithPath: args[2])
let jsonURL = URL(fileURLWithPath: args[3])
let voiceName = args.count > 4 ? args[4] : "Samantha"
let utt = AVSpeechUtterance(string: text)
if let v = AVSpeechSynthesisVoice.speechVoices().first(where: { $0.name == voiceName && $0.language.hasPrefix("en") })
    ?? AVSpeechSynthesisVoice.speechVoices().first(where: { $0.name == voiceName }) {
    utt.voice = v
}
if args.count > 5, let r = Float(args[5]) { utt.rate = r }

let synth = AVSpeechSynthesizer()
var file: AVAudioFile?
var frames: AVAudioFramePosition = 0
var rate = 22050.0
var bytesPerFrame = 2
var markers: [(Int, NSRange)] = []
var done = false
synth.write(utt, toBufferCallback: { buf in
    guard let pcm = buf as? AVAudioPCMBuffer else { return }
    if pcm.frameLength == 0 { done = true; return }
    if file == nil {
        rate = pcm.format.sampleRate
        bytesPerFrame = Int(pcm.format.streamDescription.pointee.mBytesPerFrame)
        file = try! AVAudioFile(forWriting: wavURL, settings: pcm.format.settings,
                                commonFormat: pcm.format.commonFormat, interleaved: pcm.format.isInterleaved)
    }
    try! file!.write(from: pcm)
    frames += AVAudioFramePosition(pcm.frameLength)
}, toMarkerCallback: { ms in
    for m in ms where m.mark == .word { markers.append((m.byteSampleOffset, m.textRange)) }
})
while !done { RunLoop.main.run(until: Date().addingTimeInterval(0.02)) }
file = nil

// Offsets are in bytes on some systems and frames on others: whichever fits.
let maxOff = markers.map { $0.0 }.max() ?? 0
let unit = maxOff > Int(frames) ? Double(bytesPerFrame) : 1.0
let total = Double(frames) / rate
let ns = text as NSString
var words: [[String: Any]] = []
for (i, (off, range)) in markers.enumerated() {
    let start = Double(off) / unit / rate
    let next = i + 1 < markers.count ? Double(markers[i + 1].0) / unit / rate : total
    let w = ns.substring(with: range)
    words.append(["word": w, "start": (start * 1000).rounded() / 1000, "end": (max(start + 0.05, next - 0.02) * 1000).rounded() / 1000])
}
let out = try! JSONSerialization.data(withJSONObject: ["words": words, "duration": total], options: [.prettyPrinted])
try! out.write(to: jsonURL)
print("\(words.count) words, \(String(format: "%.2f", total)) s -> \(wavURL.path), \(jsonURL.path)")
