using System.Buffers.Binary;
using System.Text;

namespace WandRebuild.Protocol;

public abstract record TrainerFrame;
public sealed record LoadingFrame : TrainerFrame;
public sealed record ReadyFrame : TrainerFrame;
public sealed record ValueFrame(string Name, double Value) : TrainerFrame;
public sealed record ValueResponseFrame(uint RequestId, uint Result) : TrainerFrame;
public sealed record BinaryFrame(string Type, byte[] Payload) : TrainerFrame;

/// <summary>
/// Reads legacy trainer pipe frames used by the installed Wand 12.30.1 renderer.
/// This codec does not start, inject, or attach to a game process.
/// </summary>
public sealed class TrainerWireProtocol
{
    private const int MaxPayloadBytes = 8 * 1024 * 1024;
    private const int MaxBufferedBytes = MaxPayloadBytes + 48;
    private byte[] _pending = new byte[4096];
    private int _count;

    public IReadOnlyList<TrainerFrame> Push(ReadOnlySpan<byte> chunk)
    {
        if (chunk.Length > MaxBufferedBytes - _count)
            throw new InvalidDataException("Trainer pipe buffer limit exceeded.");
        EnsureCapacity(_count + chunk.Length);
        chunk.CopyTo(_pending.AsSpan(_count));
        _count += chunk.Length;

        var frames = new List<TrainerFrame>();
        var offset = 0;
        while (_count - offset >= 8)
        {
            var remaining = _pending.AsSpan(offset, _count - offset);
            var kind = BinaryPrimitives.ReadUInt32LittleEndian(remaining);
            int frameLength;
            switch (kind)
            {
                case 0: // Loading
                case 1: // Ready
                    frameLength = 8;
                    break;
                case 2: // Value
                    frameLength = 56;
                    break;
                case 3: // ValueResponse
                    frameLength = 24;
                    break;
                case 4: // Buffer
                    if (remaining.Length < 48) goto Complete;
                    var payloadLength = BinaryPrimitives.ReadUInt32LittleEndian(remaining.Slice(40, 4));
                    if (payloadLength > MaxPayloadBytes)
                        throw new InvalidDataException("Trainer pipe payload limit exceeded.");
                    frameLength = 48 + (int)payloadLength;
                    break;
                default:
                    throw new InvalidDataException($"Unknown trainer pipe frame type {kind}.");
            }
            if (remaining.Length < frameLength) break;
            frames.Add(kind switch
            {
                0 => new LoadingFrame(),
                1 => new ReadyFrame(),
                2 => new ValueFrame(ReadAsciiField(remaining.Slice(16, 32)),
                    BitConverter.Int64BitsToDouble(BinaryPrimitives.ReadInt64LittleEndian(remaining.Slice(48, 8)))),
                3 => new ValueResponseFrame(BinaryPrimitives.ReadUInt32LittleEndian(remaining.Slice(8, 4)),
                    BinaryPrimitives.ReadUInt32LittleEndian(remaining.Slice(16, 4))),
                4 => new BinaryFrame(ReadAsciiField(remaining.Slice(8, 32)),
                    remaining.Slice(48, frameLength - 48).ToArray()),
                _ => throw new InvalidDataException()
            });
            offset += frameLength;
        }

    Complete:
        if (offset > 0)
        {
            System.Buffer.BlockCopy(_pending, offset, _pending, 0, _count - offset);
            _count -= offset;
        }
        return frames;
    }

    public static byte[] EncodeSetValue(uint requestId, string name, double value)
    {
        var frame = new byte[56];
        BinaryPrimitives.WriteUInt32LittleEndian(frame, 2);
        BinaryPrimitives.WriteUInt32LittleEndian(frame.AsSpan(8), requestId);
        WriteAsciiField(frame.AsSpan(16, 32), name);
        BinaryPrimitives.WriteInt64LittleEndian(frame.AsSpan(48), BitConverter.DoubleToInt64Bits(value));
        return frame;
    }

    public static byte[] EncodeBinary(string type, ReadOnlySpan<byte> payload)
    {
        if (payload.Length > MaxPayloadBytes)
            throw new ArgumentOutOfRangeException(nameof(payload));
        var frame = new byte[48 + payload.Length];
        BinaryPrimitives.WriteUInt32LittleEndian(frame, 4);
        WriteAsciiField(frame.AsSpan(8, 32), type);
        BinaryPrimitives.WriteUInt32LittleEndian(frame.AsSpan(40), (uint)payload.Length);
        payload.CopyTo(frame.AsSpan(48));
        return frame;
    }

    private static string ReadAsciiField(ReadOnlySpan<byte> field)
    {
        var end = field.IndexOf((byte)0);
        return Encoding.ASCII.GetString(end < 0 ? field : field.Slice(0, end));
    }

    private static void WriteAsciiField(Span<byte> destination, string text)
    {
        if (string.IsNullOrEmpty(text) || text.Length >= destination.Length || text.Any(c => c > 127 || c == 0))
            throw new ArgumentException("Field must contain 1–31 ASCII characters.", nameof(text));
        Encoding.ASCII.GetBytes(text.AsSpan(), destination);
    }

    private void EnsureCapacity(int needed)
    {
        if (needed <= _pending.Length) return;
        Array.Resize(ref _pending, Math.Min(MaxBufferedBytes, Math.Max(needed, _pending.Length * 2)));
    }
}
