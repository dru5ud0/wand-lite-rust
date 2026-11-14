namespace WandRebuild.Protocol;

public sealed record WandRuntime(
    Version Version,
    string AppDirectory,
    string TrainerLibraryDirectory,
    string ArchivePath);

/// <summary>Finds the installed Wand backend without changing its files.</summary>
public static class WandRuntimeLocator
{
    public static WandRuntime? FindNewest(string localAppDataDirectory)
    {
        var wandDirectory = Path.Combine(localAppDataDirectory, "Wand");
        if (!Directory.Exists(wandDirectory)) return null;

        var candidates = new List<WandRuntime>();
        foreach (var appDirectory in Directory.EnumerateDirectories(wandDirectory, "app-*"))
        {
            var name = Path.GetFileName(appDirectory);
            if (!Version.TryParse(name.AsSpan(4), out var version)) continue;
            var archivePath = Path.Combine(appDirectory, "resources", "app.asar");
            var trainerDirectory = Path.Combine(appDirectory, "resources", "app.asar.unpacked",
                "static", "unpacked", "trainerlib");
            if (!File.Exists(Path.Combine(appDirectory, "Wand.exe")) || !File.Exists(archivePath)) continue;
            var required = new[]
            {
                "TrainerHost_x64.exe", "TrainerHost_x86.exe",
                "TrainerLib_x64.dll", "TrainerLib_x86.dll",
                "TrainerLibPlugin_x64.dll", "TrainerLibPlugin_x86.dll",
                "InputCapturePlugin_x64.dll", "InputCapturePlugin_x86.dll",
                "CELib_x64.dll", "CELib_x86.dll",
                "tophat_service_x64.dll", "tophat_service_x86.dll"
            };
            if (required.Any(name => !File.Exists(Path.Combine(trainerDirectory, name)))) continue;
            candidates.Add(new WandRuntime(version, appDirectory, trainerDirectory, archivePath));
        }
        return candidates.OrderByDescending(candidate => candidate.Version).FirstOrDefault();
    }
}
