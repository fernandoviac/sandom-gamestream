using System.Collections.Generic;
using UnityEngine.InputSystem;

namespace Sandom.GameStream
{
    /// The Windows virtual-key code of each Input System key that the
    /// GameStream protocol carries (sandom_gamestream_protocol::input::Key).
    /// The Input System names keys by their position on a US layout, as
    /// virtual-key codes do.
    public static class GameStreamKeys
    {
        static readonly Dictionary<Key, byte> VirtualKeys = Build();

        public static bool TryGet(Key key, out byte virtualKey) => VirtualKeys.TryGetValue(key, out virtualKey);

        static Dictionary<Key, byte> Build()
        {
            var keys = new Dictionary<Key, byte>
            {
                [Key.Backspace] = 0x08, [Key.Tab] = 0x09, [Key.Enter] = 0x0D, [Key.NumpadEnter] = 0x0D,
                [Key.Pause] = 0x13, [Key.CapsLock] = 0x14, [Key.Escape] = 0x1B, [Key.Space] = 0x20,
                [Key.PageUp] = 0x21, [Key.PageDown] = 0x22, [Key.End] = 0x23, [Key.Home] = 0x24,
                [Key.LeftArrow] = 0x25, [Key.UpArrow] = 0x26, [Key.RightArrow] = 0x27, [Key.DownArrow] = 0x28,
                [Key.PrintScreen] = 0x2C, [Key.Insert] = 0x2D, [Key.Delete] = 0x2E,
                [Key.LeftMeta] = 0x5B, [Key.RightMeta] = 0x5C,
                [Key.NumpadMultiply] = 0x6A, [Key.NumpadPlus] = 0x6B, [Key.NumpadMinus] = 0x6D,
                [Key.NumpadPeriod] = 0x6E, [Key.NumpadDivide] = 0x6F,
                [Key.NumLock] = 0x90, [Key.ScrollLock] = 0x91,
                [Key.LeftShift] = 0xA0, [Key.RightShift] = 0xA1, [Key.LeftCtrl] = 0xA2, [Key.RightCtrl] = 0xA3,
                [Key.LeftAlt] = 0xA4, [Key.RightAlt] = 0xA5,
                [Key.Semicolon] = 0xBA, [Key.Equals] = 0xBB, [Key.Comma] = 0xBC, [Key.Minus] = 0xBD,
                [Key.Period] = 0xBE, [Key.Slash] = 0xBF, [Key.Backquote] = 0xC0, [Key.LeftBracket] = 0xDB,
                [Key.Backslash] = 0xDC, [Key.RightBracket] = 0xDD, [Key.Quote] = 0xDE, [Key.OEM1] = 0xE2,
            };
            for (int letter = 0; letter < 26; letter++) keys[Key.A + letter] = (byte)(0x41 + letter);
            keys[Key.Digit0] = 0x30;
            for (int digit = 1; digit <= 9; digit++) keys[Key.Digit1 + digit - 1] = (byte)(0x30 + digit);
            for (int digit = 0; digit <= 9; digit++) keys[Key.Numpad0 + digit] = (byte)(0x60 + digit);
            for (int function = 0; function < 12; function++) keys[Key.F1 + function] = (byte)(0x70 + function);
            return keys;
        }
    }
}
