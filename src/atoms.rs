use x11rb::atom_manager;

atom_manager! {
    pub Atoms: AtomsCookie {
        // ICCCM
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        WM_TAKE_FOCUS,
        WM_STATE,
        WM_NAME,
        WM_CLASS,
        WM_WINDOW_ROLE,
        WM_CHANGE_STATE,

        // EWMH root
        _NET_SUPPORTED,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_DESKTOP_NAMES,
        _NET_CURRENT_DESKTOP,
        _NET_ACTIVE_WINDOW,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WORKAREA,
        _NET_DESKTOP_GEOMETRY,
        _NET_DESKTOP_VIEWPORT,
        _NET_CLOSE_WINDOW,
        _NET_WM_MOVERESIZE,

        // EWMH per-client
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_STICKY,
        _NET_WM_ALLOWED_ACTIONS,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_TOOLBAR,
        _NET_WM_WINDOW_TYPE_MENU,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_WINDOW_TYPE_SPLASH,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_OPACITY,
        _NET_FRAME_EXTENTS,
        _NET_WM_STRUT_PARTIAL,

        // Actions (for _NET_WM_ALLOWED_ACTIONS)
        _NET_WM_ACTION_MOVE,
        _NET_WM_ACTION_RESIZE,
        _NET_WM_ACTION_CLOSE,
        _NET_WM_ACTION_FULLSCREEN,
        _NET_WM_ACTION_ABOVE,
        _NET_WM_ACTION_CHANGE_DESKTOP,

        // Motif
        _MOTIF_WM_HINTS,

        // UTF8
        UTF8_STRING,
    }
}
