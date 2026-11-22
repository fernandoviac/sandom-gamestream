using System;
using System.Collections;
using System.IO;
using System.Linq;
using UnityEngine;
using UnityEngine.InputSystem;
using UnityEngine.InputSystem.LowLevel;

namespace Sandom.GameStream
{
    /// The acceptance check of a player of the two-screen scene, run with
    /// `--check DIRECTORY [--pin PIN]`; without `--check` it does nothing.
    ///
    /// It waits until every screen shows a picture and captures the view
    /// (`before.png`). Then, through the Input System's event queue (the
    /// path of a real mouse and keyboard), it clicks the Name field of the
    /// `window-input` page on each screen and types a different word: "left"
    /// on the left screen, "right" on the right one; it also ticks "First" on
    /// the right screen only. After the servers' next pictures it captures
    /// the view again (`after.png`) and quits. With `--pin`, each screen
    /// pairs with that PIN, which the operator submits to each server.
    public class GameStreamSceneCheck : MonoBehaviour
    {
        /// The Name field and the "First" checkbox of window-input.html, as
        /// fractions of a 640 × 400 CSS viewport (the 1280 × 800 stream at
        /// device scale 2).
        static readonly Vector2 NameField = new Vector2(160f / 640, 58f / 400);
        static readonly Vector2 FirstCheckbox = new Vector2(24f / 640, 96f / 400);

        string directory;

        void Awake()
        {
            string[] arguments = Environment.GetCommandLineArgs();
            int check = Array.IndexOf(arguments, "--check");
            if (check < 0 || check + 1 >= arguments.Length)
            {
                enabled = false;
                return;
            }
            directory = arguments[check + 1];
            Directory.CreateDirectory(directory);
            int pin = Array.IndexOf(arguments, "--pin");
            if (pin >= 0 && pin + 1 < arguments.Length)
                foreach (GameStreamScreen screen in FindObjectsByType<GameStreamScreen>(FindObjectsSortMode.None))
                    screen.pairingPin = arguments[pin + 1];
            StartCoroutine(Run());
        }

        IEnumerator Run()
        {
            GameStreamScreen[] screens = FindObjectsByType<GameStreamScreen>(FindObjectsSortMode.None)
                .OrderBy(screen => screen.transform.position.x).ToArray();
            var shown = new bool[screens.Length];
            for (int index = 0; index < screens.Length; index++)
            {
                int captured = index;
                screens[index].PictureShown += _ => shown[captured] = true;
            }
            float deadline = Time.realtimeSinceStartup + 120;
            while (!shown.All(value => value))
            {
                if (Time.realtimeSinceStartup > deadline)
                    Fail("no picture within 120 s from " + string.Join(", ", screens.Select(screen => $"{screen.name}: {screen.CurrentState} {screen.Status}")));
                yield return null;
            }
            // The page settles after its first frames.
            yield return new WaitForSecondsRealtime(2);
            yield return Capture("before.png");

            yield return Click(screens[0], NameField);
            yield return Type("left");
            yield return Click(screens[1], NameField);
            yield return Type("right");
            yield return Click(screens[1], FirstCheckbox);
            yield return new WaitForSecondsRealtime(1.5f);
            yield return Capture("after.png");
            Debug.Log("GameStream scene check: done");
            Application.Quit(0);
        }

        void Fail(string reason)
        {
            Debug.LogError($"GameStream scene check failed: {reason}");
            Application.Quit(1);
            throw new GameStreamException(reason);
        }

        IEnumerator Capture(string name)
        {
            yield return new WaitForEndOfFrame();
            ScreenCapture.CaptureScreenshot(Path.Combine(directory, name));
            yield return null;
        }

        /// The view position of `fraction` of the page on `screen`: x from
        /// the left, y from the top.
        static Vector2 ViewPoint(GameStreamScreen screen, Vector2 fraction)
        {
            Vector3 world = screen.transform.TransformPoint(new Vector3(fraction.x - 0.5f, 0.5f - fraction.y, 0));
            return Camera.main.WorldToScreenPoint(world);
        }

        static IEnumerator Click(GameStreamScreen screen, Vector2 fraction)
        {
            Vector2 position = ViewPoint(screen, fraction);
            InputSystem.QueueStateEvent(Mouse.current, new MouseState { position = position });
            yield return null;
            InputSystem.QueueStateEvent(Mouse.current, new MouseState { position = position }.WithButton(UnityEngine.InputSystem.LowLevel.MouseButton.Left));
            yield return null;
            InputSystem.QueueStateEvent(Mouse.current, new MouseState { position = position });
            yield return null;
            yield return new WaitForSecondsRealtime(0.2f);
        }

        static IEnumerator Type(string word)
        {
            foreach (char character in word)
            {
                Key key = Key.A + (char.ToLowerInvariant(character) - 'a');
                InputSystem.QueueStateEvent(Keyboard.current, new KeyboardState(key));
                yield return null;
                InputSystem.QueueStateEvent(Keyboard.current, new KeyboardState());
                yield return null;
            }
        }
    }
}
