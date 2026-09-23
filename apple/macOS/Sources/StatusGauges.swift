import AlondraShared
import SwiftUI

/// The week's Claude usage, today against its share, memory and CPU as small gauges in the
/// toolbar; hovering one explains it in words.
struct StatusGauges: View {
    let machine: Machine
    let usage: Usage?

    var body: some View {
        HStack(spacing: 12) {
            if let usage {
                StatusGauge(
                    value: usage.weekPercent / 100,
                    label: "\(Int(usage.weekPercent.rounded()))%",
                    caption: String(localized: "Week"),
                    tint: usage.weekPercent >= 85 ? .red : .blue,
                    help: weekHelp(usage)
                )
                if let spent = usage.todaySpent, let share = usage.todayShare {
                    StatusGauge(
                        value: min(1, spent / max(share, 0.1)),
                        label: "\(Int(spent.rounded()))/\(Int(share.rounded()))%",
                        caption: String(localized: "Today"),
                        tint: usage.isOverPace ? .red : spent >= share * 0.75 ? .orange : .green,
                        help: todayHelp(spent: spent, share: share, overPace: usage.isOverPace)
                    )
                }
            }
            if let free = machine.memoryFreePercent {
                StatusGauge(
                    value: Double(100 - min(free, 100)) / 100,
                    label: "\(100 - min(free, 100))%",
                    caption: String(localized: "Memory"),
                    tint: free < 15 ? .red : free < 30 ? .orange : .teal,
                    help: memoryHelp(free: free)
                )
            }
            if let cpu = machine.cpuUsage {
                StatusGauge(
                    value: min(cpu, 100) / 100,
                    label: "\(Int(cpu.rounded()))%",
                    caption: String(localized: "CPU"),
                    tint: machine.unreliable ? .red : cpu >= 85 ? .orange : .purple,
                    help: cpuHelp(cpu: cpu)
                )
            }
        }
        .padding(.horizontal, 6)
    }

    private func weekHelp(_ u: Usage) -> String {
        let week = Int(u.weekPercent.rounded())
        let reset = Date(timeIntervalSince1970: Double(u.weekResetsAt) / 1000).formatted(.relative(presentation: .named))
        let perDay = Int(u.perDayLeft.rounded())
        return String(localized: "Claude this week: \(week)% of the weekly limit used. It resets \(reset); from here that leaves about \(perDay)% per day.")
    }

    private func todayHelp(spent: Double, share: Double, overPace: Bool) -> String {
        let text = String(localized: "Today: \(Int(spent.rounded()))% of the week spent, against the \(Int(share.rounded()))% that today's share allows.")
        guard overPace else { return text }
        return text + " " + String(localized: "Over pace: the orchestrator asks you before launching new nodes.")
    }

    private func memoryHelp(free: UInt32) -> String {
        let available = ByteCountFormatter.string(fromByteCount: Int64(machine.memoryAvailable), countStyle: .memory)
        return String(localized: "Memory in use: \(100 - min(free, 100))% (\(available) available). Below 15% free, Alondra tells the orchestrator not to launch nodes.")
    }

    private func cpuHelp(cpu: Double) -> String {
        let load = (machine.load.first ?? 0).formatted(.number.precision(.fractionLength(1)))
        let cores = machine.cpus ?? 0
        var text = String(localized: "CPU in use: \(Int(cpu.rounded()))%. Load average \(load) over \(cores) cores: how many processes want the CPU at once.")
        if machine.unreliable {
            text += " " + String(localized: "The load is so high that these readings may be wrong.")
        }
        return text
    }
}

private struct StatusGauge: View {
    let value: Double
    let label: String
    let caption: String
    let tint: Color
    let help: String

    var body: some View {
        HStack(spacing: 5) {
            Text(caption)
                .foregroundStyle(.secondary)
            Capsule()
                .fill(.quaternary)
                .frame(width: 28, height: 4)
                .overlay(alignment: .leading) {
                    Capsule()
                        .fill(tint)
                        .frame(width: 28 * max(0.06, min(value, 1)))
                }
            Text(verbatim: label)
                .monospacedDigit()
        }
        .font(.caption)
        .help(help)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(caption)
        .accessibilityValue(help)
    }
}
