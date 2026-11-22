using System.IO;
using UnityEditor;
using UnityEditor.SceneManagement;
using UnityEngine;
using UnityEngine.Rendering;
using UnityEngine.SceneManagement;

namespace Sandom.GameStream.Editor
{
    /// Builds the project's scenes and players:
    ///
    /// - `Assets/Scenes/TwoScreens.unity`: two screens side by side, angled
    ///   towards the camera. The left screen shows the server on the
    ///   GameStream ports of 127.0.0.1, the right one the server at port
    ///   offset 1000 (`sandom-stream --serve --port-offset 1000`). Both take
    ///   the player's mouse and keyboard.
    /// - `Assets/Scenes/Latency.unity`: one screen that fills the view and
    ///   echoes each shown picture to the latency tool.
    ///
    /// `-executeMethod Sandom.GameStream.Editor.GameStreamScenes.Build`
    /// writes both scenes; `BuildPlayers` also builds `Build/TwoScreens.app`
    /// and `Build/Latency.app`.
    public static class GameStreamScenes
    {
        const string ScenesDirectory = "Assets/Scenes";
        const string TwoScreensPath = ScenesDirectory + "/TwoScreens.unity";
        const string LatencyPath = ScenesDirectory + "/Latency.unity";
        const string MaterialPath = "Assets/GameStream/Screen.mat";
        /// The stream's aspect: 1280 × 800.
        const float ScreenWidth = 1.6f;
        const float ScreenHeight = 1f;

        [MenuItem("GameStream/Rebuild Scenes")]
        public static void Build()
        {
            Directory.CreateDirectory(ScenesDirectory);
            Material material = ScreenMaterial();
            BuildTwoScreens(material);
            BuildLatency(material);
            EditorBuildSettings.scenes = new[]
            {
                new EditorBuildSettingsScene(TwoScreensPath, true),
                new EditorBuildSettingsScene(LatencyPath, true),
            };
            AssetDatabase.SaveAssets();
        }

        public static void BuildPlayers()
        {
            Build();
            BuildPlayer(TwoScreensPath, "Build/TwoScreens.app");
            BuildPlayer(LatencyPath, "Build/Latency.app");
        }

        static void BuildPlayer(string scene, string location)
        {
            var report = BuildPipeline.BuildPlayer(new BuildPlayerOptions
            {
                scenes = new[] { scene },
                locationPathName = location,
                target = BuildTarget.StandaloneOSX,
                options = BuildOptions.None,
            });
            if (report.summary.result != UnityEditor.Build.Reporting.BuildResult.Succeeded)
                throw new System.Exception($"Building {location} from {scene} ended {report.summary.result} with {report.summary.totalErrors} errors");
        }

        static Material ScreenMaterial()
        {
            var material = AssetDatabase.LoadAssetAtPath<Material>(MaterialPath);
            if (material != null) return material;
            Shader shader = Shader.Find("Unlit/Texture");
            if (shader == null) throw new System.Exception("The screen material needs the built-in Unlit/Texture shader; it was not found");
            material = new Material(shader) { name = "Screen" };
            AssetDatabase.CreateAsset(material, MaterialPath);
            return material;
        }

        static Scene NewScene()
        {
            Scene scene = EditorSceneManager.NewScene(NewSceneSetup.EmptyScene, NewSceneMode.Single);
            RenderSettings.ambientMode = AmbientMode.Flat;
            RenderSettings.ambientLight = Color.white;
            return scene;
        }

        static Camera AddCamera(Vector3 position, Vector3 target)
        {
            var cameraObject = new GameObject("Main Camera") { tag = "MainCamera" };
            var camera = cameraObject.AddComponent<Camera>();
            camera.clearFlags = CameraClearFlags.SolidColor;
            camera.backgroundColor = new Color(0.08f, 0.09f, 0.11f);
            camera.transform.position = position;
            camera.transform.LookAt(target);
            camera.nearClipPlane = 0.05f;
            return camera;
        }

        static GameStreamScreen AddScreen(string name, Material material, Vector3 position, float yaw, int portOffset, bool input)
        {
            var screenObject = GameObject.CreatePrimitive(PrimitiveType.Quad);
            screenObject.name = name;
            Object.DestroyImmediate(screenObject.GetComponent<MeshCollider>());
            screenObject.transform.position = position;
            screenObject.transform.rotation = Quaternion.Euler(0, yaw, 0);
            screenObject.transform.localScale = new Vector3(ScreenWidth, ScreenHeight, 1);
            screenObject.GetComponent<MeshRenderer>().sharedMaterial = material;
            screenObject.AddComponent<MeshCollider>();
            var screen = screenObject.AddComponent<GameStreamScreen>();
            screen.address = "127.0.0.1";
            screen.portOffset = portOffset;
            screen.streamWidth = 1280;
            screen.streamHeight = 800;
            if (input) screenObject.AddComponent<GameStreamScreenInput>();
            return screen;
        }

        static void BuildTwoScreens(Material material)
        {
            Scene scene = NewScene();
            AddCamera(new Vector3(0, 0, -2.3f), new Vector3(0, 0, 0.3f));
            AddScreen("Left Screen", material, new Vector3(-0.9f, 0, 0.25f), -18, 0, true);
            AddScreen("Right Screen", material, new Vector3(0.9f, 0, 0.25f), 18, 1000, true);
            new GameObject("Scene Check").AddComponent<GameStreamSceneCheck>();
            EditorSceneManager.SaveScene(scene, TwoScreensPath);
        }

        static void BuildLatency(Material material)
        {
            Scene scene = NewScene();
            Camera camera = AddCamera(new Vector3(0, 0, -1), Vector3.zero);
            camera.orthographic = true;
            camera.orthographicSize = ScreenHeight / 2;
            GameStreamScreen screen = AddScreen("Screen", material, Vector3.zero, 0, 0, false);
            screen.streamWidth = 1280;
            screen.streamHeight = 720;
            screen.transform.localScale = new Vector3(ScreenHeight * 1280 / 720, ScreenHeight, 1);
            screen.gameObject.AddComponent<GameStreamLatencyEcho>();
            EditorSceneManager.SaveScene(scene, LatencyPath);
        }
    }
}
