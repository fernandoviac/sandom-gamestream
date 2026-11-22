using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Sandom.GameStream
{
    /// The C interface of the GameStream client (client/src/native.rs in
    /// sandom-gamestream), from `libsandom_gamestream_client`. Each call that
    /// fails throws `GameStreamException` with the library's reason.
    public static class GameStreamNative
    {
        const string Library = "sandom_gamestream_client";

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        public delegate void Doorbell(IntPtr context);

        [DllImport(Library)] static extern UIntPtr sandom_gamestream_client_last_error(byte[] buffer, UIntPtr capacity);
        [DllImport(Library)] static extern IntPtr sandom_gamestream_client_open(byte[] directory, UIntPtr length);
        [DllImport(Library)] static extern void sandom_gamestream_client_free(IntPtr client);
        [DllImport(Library)]
        static extern int sandom_gamestream_client_pair(IntPtr client, byte[] address, UIntPtr addressLength, ushort httpPort,
            byte[] pin, UIntPtr pinLength, byte[] pairingPath, UIntPtr pairingPathLength);
        [DllImport(Library)]
        static extern IntPtr sandom_gamestream_client_start(IntPtr client, byte[] pairingPath, UIntPtr pairingPathLength,
            uint width, uint height, uint fps, uint bitrateKbps, IntPtr metalDevice, Doorbell doorbell, IntPtr context);
        [DllImport(Library)]
        static extern IntPtr sandom_gamestream_client_picture(IntPtr session, out uint width, out uint height,
            out uint frameIndex, out uint rtpTimestamp);
        [DllImport(Library)] static extern IntPtr sandom_gamestream_client_picture_texture(IntPtr picture);
        [DllImport(Library)] static extern void sandom_gamestream_client_picture_free(IntPtr picture);
        [DllImport(Library)] static extern long sandom_gamestream_client_ended(IntPtr session, byte[] buffer, UIntPtr capacity);
        [DllImport(Library)] static extern int sandom_gamestream_client_key(IntPtr session, int down, byte virtualKey);
        [DllImport(Library)] static extern int sandom_gamestream_client_mouse_move(IntPtr session, short x, short y, short width, short height);
        [DllImport(Library)] static extern int sandom_gamestream_client_mouse_button(IntPtr session, int down, byte button);
        [DllImport(Library)] static extern int sandom_gamestream_client_scroll(IntPtr session, int horizontal, short amount);
        [DllImport(Library)] static extern int sandom_gamestream_client_end(IntPtr session);

        [DllImport("/usr/lib/libobjc.A.dylib")] static extern IntPtr sel_registerName(string name);
        [DllImport("/usr/lib/libobjc.A.dylib")] static extern IntPtr objc_msgSend(IntPtr receiver, IntPtr selector);

        static byte[] Utf8(string text) => Encoding.UTF8.GetBytes(text);

        /// The reason of this thread's last failed call.
        static string LastError()
        {
            int length = (int)sandom_gamestream_client_last_error(null, UIntPtr.Zero);
            var buffer = new byte[length];
            sandom_gamestream_client_last_error(buffer, (UIntPtr)length);
            return Encoding.UTF8.GetString(buffer);
        }

        static void Check(int status, string operation)
        {
            if (status != 1) throw new GameStreamException($"The GameStream client could not {operation}: {LastError()}");
        }

        /// Opens the client identity in `directory`, creating it there when it
        /// does not exist.
        public static IntPtr Open(string directory)
        {
            byte[] bytes = Utf8(directory);
            IntPtr client = sandom_gamestream_client_open(bytes, (UIntPtr)bytes.Length);
            if (client == IntPtr.Zero) throw new GameStreamException($"The GameStream client could not open its identity in {directory}: {LastError()}");
            return client;
        }

        public static void Free(IntPtr client) => sandom_gamestream_client_free(client);

        /// Pairs with the server; blocks until the user enters `pin` on the
        /// device. Saves the pairing at `pairingPath`.
        public static void Pair(IntPtr client, string address, ushort httpPort, string pin, string pairingPath)
        {
            byte[] addressBytes = Utf8(address), pinBytes = Utf8(pin), pathBytes = Utf8(pairingPath);
            Check(sandom_gamestream_client_pair(client, addressBytes, (UIntPtr)addressBytes.Length, httpPort, pinBytes,
                (UIntPtr)pinBytes.Length, pathBytes, (UIntPtr)pathBytes.Length), $"pair with {address}:{httpPort}");
        }

        /// Starts a session with the paired server; it decodes into textures of
        /// `metalDevice` and rings `doorbell` with `context` from its threads.
        public static IntPtr Start(IntPtr client, string pairingPath, int width, int height, int fps, int bitrateKbps,
            IntPtr metalDevice, Doorbell doorbell, IntPtr context)
        {
            byte[] pathBytes = Utf8(pairingPath);
            IntPtr session = sandom_gamestream_client_start(client, pathBytes, (UIntPtr)pathBytes.Length, (uint)width, (uint)height,
                (uint)fps, (uint)bitrateKbps, metalDevice, doorbell, context);
            if (session == IntPtr.Zero) throw new GameStreamException($"The GameStream client could not start a session: {LastError()}");
            return session;
        }

        /// The latest picture, or `IntPtr.Zero` before the first one. Free it
        /// with `FreePicture`.
        public static IntPtr Picture(IntPtr session, out int width, out int height, out uint frameIndex)
        {
            IntPtr picture = sandom_gamestream_client_picture(session, out uint pictureWidth, out uint pictureHeight, out frameIndex, out _);
            width = (int)pictureWidth;
            height = (int)pictureHeight;
            return picture;
        }

        /// The picture's `id<MTLTexture>` (BGRA8Unorm).
        public static IntPtr PictureTexture(IntPtr picture) => sandom_gamestream_client_picture_texture(picture);

        public static void FreePicture(IntPtr picture) => sandom_gamestream_client_picture_free(picture);

        /// Why the session ended, or null while it runs.
        public static string Ended(IntPtr session)
        {
            long length = sandom_gamestream_client_ended(session, null, UIntPtr.Zero);
            if (length < 0) return null;
            var buffer = new byte[length];
            sandom_gamestream_client_ended(session, buffer, (UIntPtr)length);
            return Encoding.UTF8.GetString(buffer);
        }

        public static void Key(IntPtr session, bool down, byte virtualKey) =>
            Check(sandom_gamestream_client_key(session, down ? 1 : 0, virtualKey), $"send key 0x{virtualKey:X2}");

        public static void MouseMove(IntPtr session, int x, int y, int width, int height) =>
            Check(sandom_gamestream_client_mouse_move(session, (short)x, (short)y, (short)width, (short)height), "move the pointer");

        /// `button` is 1 to 5: left, middle, right, side, extra.
        public static void MouseButton(IntPtr session, bool down, byte button) =>
            Check(sandom_gamestream_client_mouse_button(session, down ? 1 : 0, button), $"send mouse button {button}");

        /// `amount` is 120 per notch, positive up or right.
        public static void Scroll(IntPtr session, bool horizontal, short amount) =>
            Check(sandom_gamestream_client_scroll(session, horizontal ? 1 : 0, amount), "scroll");

        /// Ends the session on the server and frees it; blocks for the
        /// server's answer.
        public static void End(IntPtr session) => Check(sandom_gamestream_client_end(session), "end the session");

        /// The `id<MTLDevice>` of a Metal texture.
        public static IntPtr TextureDevice(IntPtr metalTexture) => objc_msgSend(metalTexture, sel_registerName("device"));
    }

    public class GameStreamException : Exception
    {
        public GameStreamException(string message) : base(message) { }
    }
}
