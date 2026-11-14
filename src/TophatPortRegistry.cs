using System.Text.Json;

namespace WandRebuild.Protocol;

public sealed record TophatServiceEntry(int Pid, int Port, string? Service, string? Target);

/// <summary>
/// Reads Tophat's local port registry. A caller must still verify that the PID is live
/// and belongs to the intended game or host before opening a connection.
/// </summary>
public static class TophatPortRegistry
{
    public static int? FindPort(string registryPath, string processName, int? processId = null)
    {
        List<TophatServiceEntry>? entries;
        try
        {
            entries = JsonSerializer.Deserialize<List<TophatServiceEntry>>(
                File.ReadAllText(registryPath),
                new JsonSerializerOptions { PropertyNameCaseInsensitive = true });
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
        {
            return null;
        }
        if (entries is null) return null;

        TophatServiceEntry? match;
        if (processId is int pid)
            match = entries.FirstOrDefault(entry => entry.Pid == pid);
        else
            match = entries.FirstOrDefault(entry =>
                string.Equals(entry.Target, processName, StringComparison.OrdinalIgnoreCase))
                ?? entries.FirstOrDefault(entry =>
                    string.Equals(entry.Service, processName, StringComparison.OrdinalIgnoreCase));
        if (match is null) return null;
        if (match.Port is < 1 or > 65535)
            throw new InvalidDataException("Tophat registry contains an invalid port.");
        return match.Port;
    }
}
