# dmgbuild settings for the Muxflow disk image: the standard drag-to-install
# window, the app beside an Applications shortcut over dmgbuild's stock arrow.
# build-package.sh passes the finished bundle as `-D app=PATH`.
import os.path

app = defines["app"]  # noqa: F821 - injected by dmgbuild
app_name = os.path.basename(app)

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(app, "Contents", "Resources", "icon.icns")

background = "builtin-arrow"
window_rect = ((200, 200), (640, 400))
icon_size = 128
icon_locations = {app_name: (140, 120), "Applications": (500, 120)}
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
