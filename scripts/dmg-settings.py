# dmgbuild settings for MagSafe-Dim.dmg, used by scripts/build-app.sh:
# the app on the left, a link to Applications on the right, an arrow between them
# (drawn in assets/dmg-background.svg). dmgbuild writes the window layout itself,
# so no Finder scripting is needed.
import os.path

application = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)
appname = os.path.basename(application)

format = "ULMO"  # LZMA: smaller than UDZO; opens on macOS 10.15 and later
files = [application]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(application, "Contents", "Resources", "AppIcon.icns")
background = defines["background"]  # noqa: F821

# The outer frame: 28 pt taller than the 400 pt background, for the title bar.
window_rect = ((200, 200), (660, 428))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

icon_size = 128
text_size = 13
icon_locations = {
    appname: (170, 150),
    "Applications": (490, 150),
    # Hidden files, seen only by people who show hidden files in Finder: just below
    # the window, on the background's continuation, so they never sit on the design
    # and scrolling down shows the same ground rather than white.
    ".background.tiff": (170, 600),
    ".VolumeIcon.icns": (330, 600),
    ".DS_Store": (490, 600),
}
hide_extension = [appname]
