using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;
using AOT;
using UnityEngine;

namespace Sandom.GameStream
{
    /// Shows the video of one GameStream server on this object's mesh.
    ///
    /// `Connect` pairs once with the server and then starts a session; the
    /// pairing is saved under `Application.persistentDataPath`, so the next
    /// connection starts the session directly. While it pairs, the screen
    /// shows the PIN to enter on the device. Each decoded picture is a Metal
    /// texture of Unity's own device, which the mesh's material shows as its
    /// main texture with no copy. `Disconnect` ends the session on the server.
    ///
    /// The pictures are BGRA8Unorm with gamma-encoded colours, which the
    /// project's Gamma colour space shows as they are.
    [RequireComponent(typeof(MeshRenderer))]
    public class GameStreamScreen : MonoBehaviour
    {
        public enum State { Idle, Pairing, Connecting, Streaming, Ended, Failed }

        [Tooltip("The server's address: a host name or an IP address.")]
        public string address = "127.0.0.1";
        [Tooltip("Added to every GameStream port of the server (sandom-stream --port-offset).")]
        public int portOffset;
        [Tooltip("The stream's size in pixels.")]
        public int streamWidth = 1280;
        public int streamHeight = 800;
        public int fps = 60;
        public int bitrateKbps = 10000;
        [Tooltip("The PIN to pair with; empty for a random one, which the screen shows.")]
        public string pairingPin = "";
        public bool connectOnStart = true;

        /// A picture became the screen's texture in this frame: its frame
        /// index.
        public event Action<uint> PictureShown;

        public State CurrentState { get; private set; } = State.Idle;
        /// What the screen shows besides video: the PIN, the progress, or why
        /// the session ended.
        public string Status { get; private set; } = "";
        /// The running session's handle, or `IntPtr.Zero`.
        public IntPtr Session => session;

        const int DefaultHttpPort = 47989;
        /// Pictures that the GPU may still sample: a picture's memory returns
        /// to the decoder's pool when its handle is freed.
        const int RetainedPictures = 3;

        static IntPtr client;
        static readonly GameStreamNative.Doorbell DoorbellCallback = Ring;

        /// Set from the session's threads when a picture arrives or the
        /// session ends; taken on the main thread.
        sealed class Bell
        {
            public int rung;
        }

        readonly Bell bell = new Bell();
        GCHandle bellHandle;
        IntPtr session;
        Texture2D texture;
        Material material;
        uint shownFrame;
        readonly Queue<IntPtr> pictures = new Queue<IntPtr>();
        TextMesh statusText;

        public int HttpPort => DefaultHttpPort + portOffset;

        string PairingPath => Path.Combine(Application.persistentDataPath, "pairings",
            $"{address.Replace(':', '_')}-{HttpPort}.pairing");

        [MonoPInvokeCallback(typeof(GameStreamNative.Doorbell))]
        static void Ring(IntPtr context)
        {
            var bell = (Bell)GCHandle.FromIntPtr(context).Target;
            Interlocked.Exchange(ref bell.rung, 1);
        }

        static IntPtr Client()
        {
            if (client == IntPtr.Zero)
            {
                client = GameStreamNative.Open(Path.Combine(Application.persistentDataPath, "identity"));
                Application.quitting += () =>
                {
                    GameStreamNative.Free(client);
                    client = IntPtr.Zero;
                };
            }
            return client;
        }

        void Awake()
        {
            if (QualitySettings.activeColorSpace != ColorSpace.Gamma)
                throw new GameStreamException("The screen's pictures are gamma-encoded BGRA8Unorm textures; " +
                    $"the project's colour space is {QualitySettings.activeColorSpace}. Set Player Settings → Color Space to Gamma.");
            material = GetComponent<MeshRenderer>().material;
            bellHandle = GCHandle.Alloc(bell);
            statusText = CreateStatusText();
        }

        void Start()
        {
            if (connectOnStart) Connect();
        }

        void OnEnable() => Application.onBeforeRender += ShowLatestPicture;

        void OnDisable() => Application.onBeforeRender -= ShowLatestPicture;

        void OnDestroy()
        {
            Disconnect();
            if (bellHandle.IsAllocated) bellHandle.Free();
        }

        /// Pairs when the server has no saved pairing, then starts a session.
        public async void Connect()
        {
            if (CurrentState == State.Pairing || CurrentState == State.Connecting || CurrentState == State.Streaming) return;
            try
            {
                IntPtr identity = Client();
                string pairingPath = PairingPath;
                if (!File.Exists(pairingPath))
                {
                    string pin = pairingPin.Length > 0 ? pairingPin : UnityEngine.Random.Range(0, 10000).ToString("D4");
                    Show(State.Pairing, $"Enter PIN {pin}\non {address}:{HttpPort}");
                    Directory.CreateDirectory(Path.GetDirectoryName(pairingPath));
                    string pairingAddress = address;
                    ushort httpPort = (ushort)HttpPort;
                    await Task.Run(() => GameStreamNative.Pair(identity, pairingAddress, httpPort, pin, pairingPath));
                }
                Show(State.Connecting, $"Connecting to {address}:{HttpPort}");
                IntPtr device = UnityMetalDevice();
                IntPtr context = GCHandle.ToIntPtr(bellHandle);
                int width = streamWidth, height = streamHeight, frames = fps, bitrate = bitrateKbps;
                IntPtr started = await Task.Run(() =>
                    GameStreamNative.Start(identity, pairingPath, width, height, frames, bitrate, device, DoorbellCallback, context));
                if (this == null)
                {
                    // The screen was destroyed while the session started.
                    await Task.Run(() => GameStreamNative.End(started));
                    return;
                }
                session = started;
                Show(State.Streaming, "");
            }
            catch (Exception error)
            {
                Show(State.Failed, error.Message);
                Debug.LogException(error, this);
            }
        }

        /// Ends the session on the server.
        public void Disconnect()
        {
            if (session == IntPtr.Zero) return;
            IntPtr ending = session;
            session = IntPtr.Zero;
            ReleasePictures();
            Show(State.Ended, "Disconnected");
            Task.Run(() => GameStreamNative.End(ending)).ContinueWith(task =>
                Debug.LogException(task.Exception), TaskContinuationOptions.OnlyOnFaulted);
        }

        /// Unity's Metal device, from the native texture of a Unity texture.
        static IntPtr UnityMetalDevice()
        {
            var probe = new Texture2D(1, 1, TextureFormat.BGRA32, false);
            try
            {
                IntPtr nativeTexture = probe.GetNativeTexturePtr();
                if (nativeTexture == IntPtr.Zero)
                    throw new GameStreamException($"Unity's {SystemInfo.graphicsDeviceType} device gave no native texture; the screen needs Metal.");
                return GameStreamNative.TextureDevice(nativeTexture);
            }
            finally
            {
                Destroy(probe);
            }
        }

        /// Takes the session's latest picture just before Unity renders.
        void ShowLatestPicture()
        {
            if (session == IntPtr.Zero || Interlocked.Exchange(ref bell.rung, 0) == 0) return;
            string ended = GameStreamNative.Ended(session);
            if (ended != null)
            {
                IntPtr finished = session;
                session = IntPtr.Zero;
                ReleasePictures();
                Show(State.Ended, $"Session ended:\n{ended}");
                Task.Run(() => GameStreamNative.End(finished)).ContinueWith(task =>
                    Debug.Log($"The ended session's /cancel failed: {task.Exception.GetBaseException().Message}"),
                    TaskContinuationOptions.OnlyOnFaulted);
                return;
            }
            IntPtr picture = GameStreamNative.Picture(session, out int width, out int height, out uint frameIndex);
            if (picture == IntPtr.Zero) return;
            if (frameIndex == shownFrame)
            {
                GameStreamNative.FreePicture(picture);
                return;
            }
            IntPtr metalTexture = GameStreamNative.PictureTexture(picture);
            if (texture == null || texture.width != width || texture.height != height)
            {
                if (texture != null) Destroy(texture);
                texture = Texture2D.CreateExternalTexture(width, height, TextureFormat.BGRA32, false, false, metalTexture);
                texture.wrapMode = TextureWrapMode.Clamp;
                texture.filterMode = FilterMode.Bilinear;
                material.mainTexture = texture;
                // A Metal texture's first row is the picture's top; a mesh's
                // v = 0 is its bottom.
                material.mainTextureScale = new Vector2(1, -1);
                material.mainTextureOffset = new Vector2(0, 1);
            }
            else
            {
                texture.UpdateExternalTexture(metalTexture);
            }
            pictures.Enqueue(picture);
            while (pictures.Count > RetainedPictures) GameStreamNative.FreePicture(pictures.Dequeue());
            shownFrame = frameIndex;
            statusText.gameObject.SetActive(false);
            PictureShown?.Invoke(frameIndex);
        }

        void ReleasePictures()
        {
            if (texture != null)
            {
                material.mainTexture = null;
                Destroy(texture);
                texture = null;
            }
            while (pictures.Count > 0) GameStreamNative.FreePicture(pictures.Dequeue());
            shownFrame = 0;
        }

        void Show(State state, string status)
        {
            CurrentState = state;
            Status = status;
            statusText.text = status;
            statusText.gameObject.SetActive(status.Length > 0 && texture == null);
        }

        /// The status line, in front of the mesh's centre, facing the same way
        /// as the mesh.
        TextMesh CreateStatusText()
        {
            var child = new GameObject("Status");
            child.transform.SetParent(transform, false);
            child.transform.localPosition = new Vector3(0, 0, -0.01f);
            var text = child.AddComponent<TextMesh>();
            text.anchor = TextAnchor.MiddleCenter;
            text.alignment = TextAlignment.Center;
            text.characterSize = 0.02f;
            text.fontSize = 64;
            text.color = Color.white;
            child.SetActive(false);
            return text;
        }
    }
}
