using System;
using System.Collections.Generic;
using UnityEngine;
using UnityEngine.InputSystem;
using UnityEngine.InputSystem.Controls;

namespace Sandom.GameStream
{
    /// Sends the player's mouse and keyboard to the server of a
    /// GameStreamScreen. Each frame a ray from the camera through the mouse
    /// hits the screen's MeshCollider; the hit's texture coordinate is the
    /// point on the device's screen, sent as an absolute pointer move in
    /// stream pixels. Buttons and the wheel go to the screen under the
    /// pointer; a pointer that pressed on a screen stays that screen's until
    /// its last button is up. A press on a screen gives it the keyboard, and
    /// a press anywhere else takes the keyboard away; the screen with the
    /// keyboard sends each key's down and up as a Windows virtual-key code.
    [RequireComponent(typeof(GameStreamScreen), typeof(MeshCollider))]
    public class GameStreamScreenInput : MonoBehaviour
    {
        [Tooltip("The camera whose view the pointer points into; the main camera when empty.")]
        public Camera pointerCamera;

        /// The screen with the keyboard: the last screen that a press hit.
        static GameStreamScreenInput keyboardFocus;

        /// The Input System's buttons and the protocol's button numbers.
        static readonly (Func<Mouse, ButtonControl> Control, byte Button)[] Buttons =
        {
            (mouse => mouse.leftButton, 1),
            (mouse => mouse.middleButton, 2),
            (mouse => mouse.rightButton, 3),
            (mouse => mouse.backButton, 4),
            (mouse => mouse.forwardButton, 5),
        };

        GameStreamScreen screen;
        MeshCollider screenCollider;
        Vector2Int lastPoint = new Vector2Int(-1, -1);
        int buttonsDown;
        /// The wheel's movement not sent yet, in protocol units (120 per
        /// notch): a trackpad moves in fractions of a notch.
        Vector2 pendingScroll;
        /// The virtual keys whose down went to the server and whose up has
        /// not.
        readonly HashSet<byte> keysDown = new HashSet<byte>();

        public bool HasKeyboard => keyboardFocus == this;

        void Start()
        {
            screen = GetComponent<GameStreamScreen>();
            screenCollider = GetComponent<MeshCollider>();
            if (InputSystem.settings.scrollDeltaBehavior != InputSettings.ScrollDeltaBehavior.UniformAcrossAllPlatforms)
                throw new GameStreamException("The screen's wheel input expects one unit per notch; the project's Input System scroll " +
                    $"delta behaviour is {InputSystem.settings.scrollDeltaBehavior}. Set it to Uniform Across All Platforms.");
        }

        void OnDisable()
        {
            if (keyboardFocus == this) keyboardFocus = null;
            buttonsDown = 0;
            pendingScroll = Vector2.zero;
            lastPoint = new Vector2Int(-1, -1);
        }

        void Update()
        {
            Camera view = pointerCamera != null ? pointerCamera : Camera.main;
            IntPtr session = screen.Session;
            Mouse mouse = Mouse.current;
            if (view == null || mouse == null || session == IntPtr.Zero) return;
            bool hit = ScreenPoint(view, mouse.position.ReadValue(), out Vector2Int point);
            foreach ((Func<Mouse, ButtonControl> control, byte _) in Buttons)
            {
                if (!control(mouse).wasPressedThisFrame) continue;
                if (hit) keyboardFocus = this;
                else if (keyboardFocus == this) keyboardFocus = null;
            }
            if (hit && point != lastPoint)
            {
                GameStreamNative.MouseMove(session, point.x, point.y, screen.streamWidth, screen.streamHeight);
                lastPoint = point;
            }
            for (int index = 0; index < Buttons.Length; index++)
            {
                ButtonControl control = Buttons[index].Control(mouse);
                int bit = 1 << index;
                if (control.wasPressedThisFrame && hit && (buttonsDown & bit) == 0)
                {
                    buttonsDown |= bit;
                    GameStreamNative.MouseButton(session, true, Buttons[index].Button);
                }
                else if (control.wasReleasedThisFrame && (buttonsDown & bit) != 0)
                {
                    buttonsDown &= ~bit;
                    GameStreamNative.MouseButton(session, false, Buttons[index].Button);
                }
            }
            if (hit) SendScroll(session, mouse.scroll.ReadValue());
            if (keyboardFocus == this) SendKeys(session);
            else ReleaseKeys(session);
        }

        /// The stream pixel under `screenPosition`, when the ray hits this
        /// screen: pixel row 0 is at the top of the texture, at v = 1.
        bool ScreenPoint(Camera view, Vector2 screenPosition, out Vector2Int point)
        {
            point = default;
            Ray ray = view.ScreenPointToRay(screenPosition);
            if (!screenCollider.Raycast(ray, out RaycastHit hit, view.farClipPlane)) return false;
            Vector2 uv = hit.textureCoord;
            point = new Vector2Int(
                Mathf.Clamp(Mathf.FloorToInt(uv.x * screen.streamWidth), 0, screen.streamWidth - 1),
                Mathf.Clamp(Mathf.FloorToInt((1 - uv.y) * screen.streamHeight), 0, screen.streamHeight - 1));
            return true;
        }

        void SendScroll(IntPtr session, Vector2 scroll)
        {
            pendingScroll += scroll * 120;
            short vertical = (short)Mathf.Clamp(Mathf.Round(pendingScroll.y), short.MinValue, short.MaxValue);
            short horizontal = (short)Mathf.Clamp(Mathf.Round(pendingScroll.x), short.MinValue, short.MaxValue);
            if (vertical != 0)
            {
                GameStreamNative.Scroll(session, false, vertical);
                pendingScroll.y -= vertical;
            }
            if (horizontal != 0)
            {
                GameStreamNative.Scroll(session, true, horizontal);
                pendingScroll.x -= horizontal;
            }
        }

        void SendKeys(IntPtr session)
        {
            Keyboard keyboard = Keyboard.current;
            if (keyboard == null) return;
            foreach (KeyControl control in keyboard.allKeys)
            {
                if (control == null || !GameStreamKeys.TryGet(control.keyCode, out byte virtualKey)) continue;
                if (control.wasPressedThisFrame && keysDown.Add(virtualKey)) GameStreamNative.Key(session, true, virtualKey);
                else if (control.wasReleasedThisFrame && keysDown.Remove(virtualKey)) GameStreamNative.Key(session, false, virtualKey);
            }
        }

        /// Releases the keys that are down on the server: the keyboard went
        /// to another screen or none.
        void ReleaseKeys(IntPtr session)
        {
            foreach (byte virtualKey in keysDown) GameStreamNative.Key(session, false, virtualKey);
            keysDown.Clear();
        }
    }
}
