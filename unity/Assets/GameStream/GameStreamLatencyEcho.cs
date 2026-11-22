using System;
using System.Collections;
using UnityEngine;

namespace Sandom.GameStream
{
    /// Measures with sandom-gamestream's latency tool (`cargo run --release
    /// -p sandom-gamestream-client --example latency serve ...`): after a
    /// frame that showed a new picture has rendered, it sends an absolute
    /// pointer move whose x is that picture's frame index, and the tool times
    /// each frame from its send to that move's arrival.
    ///
    /// A player reads `--server ADDRESS`, `--port-offset N` and `--pin PIN`
    /// from its command line, connects, and quits when the session ends.
    [RequireComponent(typeof(GameStreamScreen))]
    public class GameStreamLatencyEcho : MonoBehaviour
    {
        GameStreamScreen screen;
        uint shownFrame;
        uint echoedFrame;
        readonly WaitForEndOfFrame endOfFrame = new WaitForEndOfFrame();

        void Awake()
        {
            screen = GetComponent<GameStreamScreen>();
            screen.connectOnStart = false;
            string[] arguments = Environment.GetCommandLineArgs();
            for (int index = 0; index + 1 < arguments.Length; index++)
            {
                string value = arguments[index + 1];
                switch (arguments[index])
                {
                    case "--server": screen.address = value; break;
                    case "--port-offset": screen.portOffset = int.Parse(value); break;
                    case "--pin": screen.pairingPin = value; break;
                }
            }
            screen.PictureShown += frame => shownFrame = frame;
        }

        IEnumerator Start()
        {
            screen.Connect();
            while (true)
            {
                yield return endOfFrame;
                if (shownFrame != echoedFrame && screen.Session != IntPtr.Zero)
                {
                    GameStreamNative.MouseMove(screen.Session, (int)shownFrame, 0, screen.streamWidth, screen.streamHeight);
                    echoedFrame = shownFrame;
                }
                if (!Application.isEditor && (screen.CurrentState == GameStreamScreen.State.Ended || screen.CurrentState == GameStreamScreen.State.Failed))
                {
                    Debug.Log($"GameStream latency echo: {screen.Status}");
                    Application.Quit(screen.CurrentState == GameStreamScreen.State.Failed ? 1 : 0);
                    yield break;
                }
            }
        }
    }
}
