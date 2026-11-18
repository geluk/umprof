
# How to use

`umprof` works by patching Mono (Unity's C# runtime) to write the memory addresses of JIT-compiled C# methods to a file.
A Windows Performance Recorder profile can then be correlated with this JIT map to resolve execution times of C# code.

Several components are needed to capture and analyse a profile. To get them all, download the latest ZIP package from
the [releases](https://github.com/geluk/umprof/releases) page.

The general profiling process looks like this:

1. Start your game with the `MONO_JIT_MAP_DIR` environment variable set.
2. Capture a profile with WPR.
3. Correlate the profile with `umprof.exe`
4. Open the result in your web browser to view it.


## First-time setup

To capture C# stacks, it is necessary to patch the mono runtime to write JIT memory addresses to a file. This patch is
available [here](https://github.com/geluk/mono/releases). Follow the instructions in the release notes to install the
patch.

The profiling process involves creating and modifying several files. It is recommended to create a working directory on
a disk with at least several gigabytes of space available (e.g. `E:\profiles`. We will use this as our example path 
in the instructions below).

Extract the `umprof` release ZIP to your working directory, then follow the instructions below.

## To capture a profile

To capture a profile, the game must be instructed to write JIT maps to a file. This is not done by default, as it has
a slight negative performance impact, especially during startup. The Mono runtime will write JIT maps only when the
appropriate environment variable is set.

Because of this, we will set the environment variable locally, so JIT maps are
only written when you launch the game from your shell.

Open a PowerShell, set the environment variable, and launch the patched game:
```pwsh
$env:MONO_JIT_MAP_DIR="E:\profiles"
E:\Path\To\Game.exe
```

Take a look at `E:\profiles`, if you did everything right, a file named `mono-jit-<PID>.map` will be created, with PID
being the game's process ID.

In the game, bring everything into a state where you are ready to start capturing the profile. Load a save, go to the
right place, etc...

Capturing a profile must be done from a shell with Administrator permissions, or ETW will refuse to run.
Open an administrator PowerShell, navigate to your working directory, and start recording.

```pwsh
cd E:\profiles
.\record.ps1 -Name mytrace -Duration 60
```

This will run the recorder for 60 seconds. Alt-tab back into your game, and test whatever you want to test.
If you do not specify a duration, the recorder will continue running until you press `Q` in the shell.

When finished, the recorder will write a file named `mytrace.etl` to your working directory. Together with the JIT map,
`umprof` will use this to generate a performance profile.

Point `umprof` at the created file to generate the profile.
```pwsh
.\umprof.exe mytrace.etl
```

This will take a moment, especially on first run, as debugging symbols will be downloaded from Microsoft's and Unity's
symbol servers. If there are any issues, they will show up in the log as warning messages. Informational messages such
as 'no symbols for <DLL>' can generally be ignored, as long as they do not refer to any DLL you're interested in.

When finished, the profile will be written to `out\<name>.firefox.json`.

## Reading the results

The results are written to a JSON file that can be understood by the Firefox Profiler. This is a web app in which you
can open the profile to view it. The profile viewer is entirely in-browser, with no data being uploaded by
Firefox's servers (although you can choose to upload a file to share it).

Go to https://profiler.firefox.com/ and click 'Load a profile from file'. Navigate to your JSON file and select it.
This opens the profile viewer.

In the split screen, at the top you can view a timeline of CPU usage, by thread. Click on a thread to select it. You
will almost always only want to look at the main thread, which sits at the top and is selected by default.

The other half of the split has several tabs. Most of the time you will want to view the 'Flame Graph' tab. Open it to
get a complete overview of all the code the main thread spends its time on.

To read a flame graph, you should know that each box represents a function that the CPU spent time on. If the function
calls another function, that function will be placed on top of the box. The width of the box indicates how much time
the CPU spent execution this function. It follows that the time spent on called functions can never exceed the time
spent in the calling function, so as you move up through the stack, the boxes gradually narrow.

By default, the entire game update loop is shown across the screen. This includes a lot of engine code that you have no
influence over, so it helps to narrow things down a bit. Somewhere at the left, you will normally see a blue frame named
`UnityPlayer.dll!BaseBehaviourManager::CommonUpdate<BehaviourManager>()`. This is the bit of Unity Engine code that
goes through all game objects, calling `Update()` on each of them, and is usually a good starting point.

Let's select it, pressing `Shift + F` to focus only this function. Above the frame, you will see a large number of
yellow frames, representing C# code that gets executed as a result of the update. Look for anything you're interested
in, and press `Shift + F` again if you need to zoom in further.

Sometimes, you only want to view the performance during a specific time range. You can do this by clicking and dragging
in the timeline at the top, to select the time range you're interested in. The view at the bottom will automatically
update to reflect this.

At a glance, the colour of frames can be used to identify their origin: kernel frames are red, native frames are blue,
and managed (C#) frames are yellow. Any Harmony-patched methods will show up as green frames. Orange frames are wrappers
emitted by Mono to handle the transition in and out of managed code.

### Graphics debugging

Some basic graphics information is included as well. Zoomed in on a trace, in the thread timeline at the top of
the screen, you can see grey ticks marking the moment when the CPU has finished rendering a frame. Hover over them to
view frame time and frame rate at that moment.

At the bottom of the thread list, there is also a chart that shows frame rate over time. Note that this is the
instantaneous frame rate, calculated from the current frame time, without averaging.

## Advanced functionality

### Profiling native code

If your code calls into a native DLL, you can include its symbols into the trace as well. Pass `--symbols-dir <path>`
to include an additional path containing PDB files. This parameter can be repeated multiple times to include additional
paths.

### Custom markers

WIP. In a future version, it will be possible to reference `UmprofCompanion` from C# code, and call its APIs to emit
custom markers at specific points in time, to aid in selecting a time range for profiling.
