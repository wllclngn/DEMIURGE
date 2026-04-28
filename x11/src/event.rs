use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

use crate::keys;
use crate::layout::Layout;
use crate::mouse;
use crate::mru;
use crate::wm::Wm;

pub fn handle(wm: &mut Wm, event: Event) {
    match event {
        Event::MapRequest(ev) => on_map_request(wm, ev),
        Event::UnmapNotify(ev) => on_unmap_notify(wm, ev),
        Event::DestroyNotify(ev) => on_destroy_notify(wm, ev),
        Event::ConfigureRequest(ev) => on_configure_request(wm, ev),
        Event::KeyPress(ev) => on_key_press(wm, ev),
        Event::KeyRelease(ev) => on_key_release(wm, ev),
        Event::ButtonPress(ev) => on_button_press(wm, ev),
        Event::ButtonRelease(_) => on_button_release(wm),
        Event::MotionNotify(ev) => on_motion_notify(wm, ev),
        Event::ClientMessage(ev) => wm.handle_client_message(&ev),
        Event::ConfigureNotify(_) => {}
        Event::MappingNotify(ev) => {
            // Pointer remap doesn't affect keyboard cache
            if ev.request != Mapping::POINTER {
                wm.rebuild_keymap();
            }
        }
        Event::PropertyNotify(ev) => on_property_notify(wm, ev),
        Event::Expose(ev) => {
            if let Some(ref mut bar) = wm.bar
                && bar.contains_window(ev.window)
                && ev.count == 0
            {
                // Bar window was exposed; full repaint into the
                // affected panel. Per-rect Expose handling is a
                // post-v0.4.1 nice-to-have.
                bar.mark_all_dirty();
            }
        }
        // RandR change notifies. xrandr emits a flurry (CRTC + Output +
        // ScreenChange) per invocation; we just set a dirty flag and let
        // main::run drain everything before calling refresh_monitors once.
        Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
            wm.monitors_dirty = true;
        }
        _ => {}
    }
}

fn on_map_request(wm: &mut Wm, ev: MapRequestEvent) {
    if let Ok(reply) = wm.conn.get_window_attributes(ev.window) {
        if let Ok(attrs) = reply.reply() {
            if attrs.override_redirect {
                return;
            }
        }
    }

    wm.manage(ev.window);

    // Only map the window if its tag is currently visible on some
    // monitor. Tag-targeted spawns may route a new window to a tag
    // that no monitor is showing -- leave those unmapped until a
    // monitor views the target tag.
    let visible = wm
        .clients
        .iter()
        .find(|c| c.window == ev.window)
        .map_or(true, |c| wm.tag_visible(c.tag));
    if visible {
        let _ = wm.conn.map_window(ev.window);
    }
    let _ = wm.conn.flush();
}

fn on_unmap_notify(wm: &mut Wm, ev: UnmapNotifyEvent) {
    if ev.event != wm.root {
        return;
    }
    // Swallow the UnmapNotify echoed back from our own unmap_window calls
    // (tag switch, move-to-tag). Only real client withdraws — which arrive
    // with unmap_ignore == 0 — cause unmanage.
    if let Some(client) = wm.clients.iter_mut().find(|c| c.window == ev.window) {
        if client.unmap_ignore > 0 {
            client.unmap_ignore -= 1;
            return;
        }
    }
    wm.unmanage(ev.window);
}

fn on_destroy_notify(wm: &mut Wm, ev: DestroyNotifyEvent) {
    wm.unmanage(ev.window);
}

fn on_configure_request(wm: &mut Wm, ev: ConfigureRequestEvent) {
    if let Some(client) = wm.clients.iter().find(|c| c.window == ev.window) {
        if client.fullscreen {
            return;
        }

        // In tiled/monocle, send synthetic ConfigureNotify instead of reconfiguring
        let tag_layout = wm.layouts[client.tag];
        if tag_layout != Layout::Floating && !client.floating {
            let _ = wm.conn.send_event(
                false,
                ev.window,
                EventMask::STRUCTURE_NOTIFY,
                ConfigureNotifyEvent {
                    response_type: 22,
                    sequence: 0,
                    event: ev.window,
                    window: ev.window,
                    above_sibling: 0,
                    x: client.x as i16,
                    y: client.y as i16,
                    width: client.w as u16,
                    height: client.h as u16,
                    border_width: 0,
                    override_redirect: false,
                },
            );
            return;
        }
    }

    let mut aux = ConfigureWindowAux::from_configure_request(&ev);

    // Respect bar reservation for managed windows
    if wm.clients.iter().any(|c| c.window == ev.window) {
        if let Some(y) = aux.y {
            if y < wm.bar_height as i32 {
                aux.y = Some(wm.bar_height as i32);
            }
        }
    }

    let _ = wm.conn.configure_window(ev.window, &aux);
    let _ = wm.conn.flush();
}

fn on_key_press(wm: &mut Wm, ev: KeyPressEvent) {
    // If prompt is active, route keys to prompt handler
    if wm.bar.as_ref().map_or(false, |b| b.prompt.is_some()) {
        let keysym = keys::keymap_keysym(&wm.keymap, ev.detail, ev.state.into());
        if let Some(ref mut bar) = wm.bar {
            match bar.handle_prompt_key(keysym) {
                Some(cmd) => {
                    let _ = wm.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
                    crate::spawn::spawn(&cmd);
                }
                None => {
                    if bar.prompt.is_none() {
                        // Prompt was cancelled (Escape/empty Enter)
                        let _ = wm.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
                    }
                }
            }
        }
        // Prompt input changed, or prompt just closed; repaint the
        // prompt region (commit happens at the end of this poll
        // iteration in main.rs::run).
        if let Some(ref mut bar) = wm.bar {
            bar.mark_prompt_dirty();
        }
        return;
    }

    if let Some(action) = keys::lookup(&wm.bindings, ev.detail, ev.state.into()) {
        let action = action.clone();
        wm.handle_action(&action);
    }
}

fn on_key_release(wm: &mut Wm, ev: KeyReleaseEvent) {
    if wm.mru_cycle.is_some() {
        mru::on_key_release(wm, &ev);
    }
}

fn on_property_notify(wm: &mut Wm, ev: PropertyNotifyEvent) {
    if ev.atom != wm.atoms._NET_WM_NAME && ev.atom != wm.atoms.WM_NAME {
        return;
    }
    // Refresh the cached title (even for unfocused clients) so the next
    // focus flip displays the current value without an extra roundtrip.
    let new_title = wm.get_title(ev.window);
    if let Some(client) = wm.clients.iter_mut().find(|c| c.window == ev.window) {
        client.title = new_title;
    }
    if Some(ev.window) == wm.focus
        && let Some(ref mut bar) = wm.bar
    {
        bar.mark_title_dirty();
    }
}

fn on_button_press(wm: &mut Wm, ev: ButtonPressEvent) {
    // Bar tag clicks. The clicked panel's monitor becomes focused;
    // then view_tag operates on that monitor's slot. This is how
    // multi-monitor users redirect a tag to a specific physical screen
    // -- click that screen's bar.
    if let Some(panel_idx) = wm
        .bar
        .as_ref()
        .filter(|b| b.contains_window(ev.event) && ev.detail == 1)
        .and_then(|b| b.panel_index(ev.event))
    {
        let x = ev.event_x as i32;
        let tag = wm
            .bar
            .as_ref()
            .and_then(|b| b.tag_extents.iter().position(|&(s, e)| x >= s && x < e));
        if let Some(tag) = tag {
            if panel_idx < wm.monitors.len() {
                wm.focused_monitor = panel_idx;
            }
            wm.view_tag(tag);
            return;
        }
    }

    // For root grabs (Super+drag) the target is ev.child; for per-window
    // click-to-focus grabs the target is ev.event itself.
    let window = if ev.event == wm.root { ev.child } else { ev.event };
    if window == 0 || !wm.clients.iter().any(|c| c.window == window) {
        // Unknown window: still need to release any sync-mode grab
        let _ = wm.conn.allow_events(Allow::REPLAY_POINTER, x11rb::CURRENT_TIME);
        let _ = wm.conn.flush();
        return;
    }

    // Super+Button1 = move, Super+Button3 = resize
    let super_held = u16::from(ev.state) & u16::from(ModMask::M4) != 0;
    if super_held && (ev.detail == 1 || ev.detail == 3) {
        mouse::start_drag(wm, window, &ev);
        return;
    }

    // Click-to-focus
    wm.focus_window(Some(window));

    // Replay the click so the application receives it
    let _ = wm.conn.allow_events(Allow::REPLAY_POINTER, x11rb::CURRENT_TIME);
    let _ = wm.conn.flush();
}

fn on_button_release(wm: &mut Wm) {
    if wm.drag.is_some() {
        mouse::end_drag(wm);
    }
}

fn on_motion_notify(wm: &mut Wm, ev: MotionNotifyEvent) {
    if wm.drag.is_none() {
        return;
    }

    // Motion compression: drain queued motion events, keep only the latest.
    // Without this, every X11 motion notify (1000Hz with modern mice) causes a
    // configure_window + flush, and the cursor outpaces the window during drag.
    let mut latest = ev;
    let mut deferred: Vec<Event> = Vec::new();
    while let Ok(Some(next)) = wm.conn.poll_for_event() {
        match next {
            Event::MotionNotify(m) => latest = m,
            other => deferred.push(other),
        }
    }

    mouse::on_motion(wm, &latest);

    for ev in deferred {
        handle(wm, ev);
    }
}
