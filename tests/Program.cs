using System.Buffers.Binary;
using WandRebuild.Protocol;

static void Check(bool condition, string message)
{
    if (!condition) throw new Exception(message);
}

var codec = new TrainerWireProtocol();
var ready = new byte[8];
BinaryPrimitives.WriteUInt32LittleEndian(ready, 1);
Check(codec.Push(ready.AsSpan(0, 3)).Count == 0, "Partial header was decoded.");
var readyFrames = codec.Push(ready.AsSpan(3));
Check(readyFrames.Count == 1 && readyFrames[0] is ReadyFrame, "Ready frame was not decoded.");

var set = TrainerWireProtocol.EncodeSetValue(42, "godmode", 1.5);
Check(BinaryPrimitives.ReadUInt32LittleEndian(set) == 2, "SetValue type changed.");
Check(BinaryPrimitives.ReadUInt32LittleEndian(set.AsSpan(8)) == 42, "Request ID changed.");
Check(BitConverter.ToDouble(set, 48) == 1.5, "SetValue payload changed.");

var value = new byte[56];
BinaryPrimitives.WriteUInt32LittleEndian(value, 2);
System.Text.Encoding.ASCII.GetBytes("health").CopyTo(value, 16);
BinaryPrimitives.WriteInt64LittleEndian(value.AsSpan(48), BitConverter.DoubleToInt64Bits(99.25));
var response = new byte[24];
BinaryPrimitives.WriteUInt32LittleEndian(response, 3);
BinaryPrimitives.WriteUInt32LittleEndian(response.AsSpan(8), 42);
BinaryPrimitives.WriteUInt32LittleEndian(response.AsSpan(16), 0);
var combined = value.Concat(response).ToArray();
var valueFrames = codec.Push(combined.AsSpan(0, 60));
Check(valueFrames.Count == 1 && valueFrames[0] is ValueFrame { Name: "health", Value: 99.25 },
    "Value or frame boundary was decoded incorrectly.");
var responseFrames = codec.Push(combined.AsSpan(60));
Check(responseFrames.Count == 1 && responseFrames[0] is ValueResponseFrame { RequestId: 42, Result: 0 },
    "ValueResponse was decoded incorrectly.");

var binary = TrainerWireProtocol.EncodeBinary("ping", new byte[] { 1, 2, 3 });
Check(codec.Push(binary.AsSpan(0, 47)).Count == 0, "Partial binary header was decoded.");
var binaryFrames = codec.Push(binary.AsSpan(47));
Check(binaryFrames.Count == 1 && binaryFrames[0] is BinaryFrame { Type: "ping", Payload.Length: 3 },
    "Binary payload was decoded incorrectly.");

var oversized = new byte[48];
BinaryPrimitives.WriteUInt32LittleEndian(oversized, 4);
BinaryPrimitives.WriteUInt32LittleEndian(oversized.AsSpan(40), 8 * 1024 * 1024 + 1);
try
{
    codec.Push(oversized);
    throw new Exception("Oversized trainer payload was accepted.");
}
catch (InvalidDataException) { }

var directory = Path.Combine(Path.GetTempPath(), "WandRebuild-test-" + Guid.NewGuid().ToString("N"));
try
{
    static void MakeRuntime(string root, string version, bool complete)
    {
        var app = Path.Combine(root, "Wand", "app-" + version);
        var trainer = Path.Combine(app, "resources", "app.asar.unpacked", "static", "unpacked", "trainerlib");
        Directory.CreateDirectory(trainer);
        File.WriteAllBytes(Path.Combine(app, "Wand.exe"), Array.Empty<byte>());
        File.WriteAllBytes(Path.Combine(app, "resources", "app.asar"), Array.Empty<byte>());
        if (!complete) return;
        foreach (var name in new[] { "TrainerHost_x64.exe", "TrainerHost_x86.exe",
            "TrainerLib_x64.dll", "TrainerLib_x86.dll",
            "TrainerLibPlugin_x64.dll", "TrainerLibPlugin_x86.dll",
            "InputCapturePlugin_x64.dll", "InputCapturePlugin_x86.dll",
            "CELib_x64.dll", "CELib_x86.dll",
            "tophat_service_x64.dll", "tophat_service_x86.dll" })
            File.WriteAllBytes(Path.Combine(trainer, name), Array.Empty<byte>());
    }
    MakeRuntime(directory, "12.30.1", true);
    MakeRuntime(directory, "12.31.0", false);
    Check(WandRuntimeLocator.FindNewest(directory)?.Version == new Version(12, 30, 1),
        "Runtime locator selected an incomplete update.");
}
finally
{
    if (Directory.Exists(directory)) Directory.Delete(directory, recursive: true);
}

var registry = Path.Combine(Path.GetTempPath(), "WandRebuild-registry-" + Guid.NewGuid().ToString("N") + ".json");
try
{
    File.WriteAllText(registry, "[{\"pid\":12,\"port\":50123,\"service\":\"tophat_service\",\"target\":\"Stick It To The Stickman\"}," +
        "{\"pid\":13,\"port\":50124,\"service\":\"tophat_service\",\"target\":\"Other Game\"}]");
    Check(TophatPortRegistry.FindPort(registry, "stick it to the stickman") == 50123,
        "Tophat target name lookup failed.");
    Check(TophatPortRegistry.FindPort(registry, "ignored", 13) == 50124,
        "Tophat PID lookup failed.");
    Check(TophatPortRegistry.FindPort(registry, "ignored", 99) is null,
        "Tophat PID lookup selected an unrelated service.");
    File.WriteAllText(registry, "[{\"pid\":12,\"port\":70000,\"target\":\"Game\"}]");
    try
    {
        TophatPortRegistry.FindPort(registry, "Game");
        throw new Exception("Invalid Tophat port was accepted.");
    }
    catch (InvalidDataException) { }
}
finally
{
    if (File.Exists(registry)) File.Delete(registry);
}

Console.WriteLine("Wand backend contract checks passed.");
