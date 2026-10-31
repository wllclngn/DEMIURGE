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
                // A bar window was exposed; every panel is put again
                // whole at the next commit.
                bar.expose();
            }
            if wm.tag_menu.as_ref().is_some_and(|m| m.window == ev.window) && ev.count == 0 {
                wm.redraw_tag_menu();
            }
            if ev.count == 0
                && let Some(pos) = wm.frame_owner(ev.window)
            {
                wm.draw_frame(pos);
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

    // A new window waits for cherrypie's answer before it maps; one already
    // waiting, or already managed (a client re-mapping itself), is not asked
    // about again.
    if wm.policy.is_waiting(ev.window) {
        return;
    }
    if !wm.clients.iter().any(|c| c.window == ev.window) && wm.ask_policy(ev.window) {
        return;
    }
    wm.admit(ev.window, None);
}

fn on_unmap_notify(wm: &mut Wm, ev: UnmapNotifyEvent) {
    // A client's unmap is read where its parent reports it: The root, or
    // the client's frame. The copy the client's own window receives, and a
    // frame's unmap reported to the root, are not withdraws.
    let in_frame = wm
        .frame_owner(ev.event)
        .is_some_and(|pos| wm.clients[pos].window == ev.window);
    if ev.event != wm.root && !in_frame {
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
    wm.policy.forget(ev.window);
    wm.unmanage(ev.window);
}

fn on_configure_request(wm: &mut Wm, ev: ConfigureRequestEvent) {
    if let Some(client) = wm.clients.iter().find(|c| c.window == ev.window) {
        if client.fullscreen {
            return;
        }

        // A framed window asks for its own size: The frame grows by the
        // title bar, and the frame goes where the window asks to be.
        if client.frame.is_some()
            && let Some(pos) = wm.clients.iter().position(|c| c.window == ev.window)
        {
            let bar = wm.decor(pos);
            let c = &wm.clients[pos];
            let aux = ConfigureWindowAux::from_configure_request(&ev);
            let x = aux.x.unwrap_or(c.x);
            let y = aux.y.map_or(c.y, |y| y.max(wm.bar_height as i32));
            let w = aux.width.unwrap_or(c.w);
            let h = aux.height.map_or(c.h, |h| h + bar);
            wm.set_geometry(pos, x, y, w, h);
            if let Some(stack) = aux.stack_mode {
                let top = wm.top_window(pos);
                let _ = wm.conn.configure_window(top, &ConfigureWindowAux::new().stack_mode(stack));
            }
            let _ = wm.conn.flush();
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

    // Respect bar reservation for managed windows, and keep the client's
    // recorded geometry in step with what is granted: Drags, the
    // tile-to-floating restore and leaving fullscreen all start from it.
    let bar_height = wm.bar_height as i32;
    if let Some(client) = wm.clients.iter_mut().find(|c| c.window == ev.window) {
        if let Some(y) = aux.y {
            if y < bar_height {
                aux.y = Some(bar_height);
            }
        }
        if let Some(x) = aux.x {
            client.x = x;
        }
        if let Some(y) = aux.y {
            client.y = y;
        }
        if let Some(w) = aux.width {
            client.w = w;
        }
        if let Some(h) = aux.height {
            client.h = h;
        }
    }

    let _ = wm.conn.configure_window(ev.window, &aux);
    let _ = wm.conn.flush();
}

fn on_key_press(wm: &mut Wm, ev: KeyPressEvent) {
    // The tag menu holds the keyboard while open.
    if let Some(menu) = wm.tag_menu.as_mut() {
        match keys::keymap_keysym(&wm.keymap, ev.detail, ev.state.into()) {
            // Up, Down
            0xff52 => menu.step(false),
            0xff54 => menu.step(true),
            // Return, KP_Enter
            0xff0d | 0xff8d => {
                if let Some(i) = menu.selected {
                    wm.pick_tag_menu(i);
                }
                return;
            }
            // Escape
            0xff1b => {
                wm.close_tag_menu();
                return;
            }
            _ => return,
        }
        wm.redraw_tag_menu();
        return;
    }

    // If prompt is active, route keys to prompt handler
    if wm.bar.as_ref().map_or(false, |b| b.prompt.is_some()) {
        let keysym = keys::keymap_keysym(&wm.keymap, ev.detail, ev.state.into());
        if let Some(ref mut bar) = wm.bar {
            match bar.handle_prompt_key(keysym) {
                Some(cmd) => {
                    let _ = wm.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
                    demiurge_core::spawn::spawn(&cmd);
                }
                None => {
                    if bar.prompt.is_none() {
                        // Prompt was cancelled (Escape/empty Enter)
                        let _ = wm.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
                    }
                }
            }
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
    if ev.atom == u32::from(AtomEnum::WM_HINTS) {
        let urgent = wm.hints_urgent(ev.window);
        wm.set_urgent(ev.window, urgent);
        return;
    }
    if ev.atom != wm.atoms._NET_WM_NAME && ev.atom != wm.atoms.WM_NAME {
        return;
    }
    // Refresh the cached title (even for unfocused clients) so the next
    // focus flip displays the current value without an extra roundtrip.
    let new_title = wm.get_title(ev.window);
    if let Some(pos) = wm.clients.iter().position(|c| c.window == ev.window) {
        wm.clients[pos].title = new_title;
        wm.draw_frame(pos);
    }
}

fn on_button_press(wm: &mut Wm, ev: ButtonPressEvent) {
    // The tag menu holds the pointer while open: A click on an entry picks
    // it, a click anywhere else closes the menu.
    if let Some(menu) = wm.tag_menu.as_ref() {
        match menu.entry_at(ev.root_x as i32, ev.root_y as i32) {
            Some(i) => wm.pick_tag_menu(i),
            None => wm.close_tag_menu(),
        }
        return;
    }

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
            .and_then(|b| b.tag_extents().iter().position(|&(s, e)| x >= s && x < e));
        if let Some(tag) = tag {
            if panel_idx < wm.monitors.len() {
                wm.focused_monitor = panel_idx;
            }
            // The tag already in view on this panel opens its menu.
            if wm.active_tags.get(panel_idx) == Some(&tag) {
                wm.open_tag_menu(panel_idx, false);
            } else {
                wm.view_tag(tag);
            }
            return;
        }
    }

    // A press on a frame's title bar, its lights and its drag.
    if let Some(pos) = wm.frame_owner(ev.event) {
        wm.on_frame_press(pos, &ev);
        let _ = wm.conn.flush();
        return;
    }

    // For root grabs (Super+drag) the target is ev.child, a frame for a
    // framed window; for per-window click-to-focus grabs the target is
    // ev.event itself.
    let target = if ev.event == wm.root { ev.child } else { ev.event };
    let window = wm.client_of(target).map_or(target, |pos| wm.clients[pos].window);
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
    // Hovering the tag menu selects the entry under the pointer.
    if let Some(menu) = wm.tag_menu.as_mut() {
        let hovered = menu.entry_at(ev.root_x as i32, ev.root_y as i32);
        if hovered.is_some() && hovered != menu.selected {
            menu.selected = hovered;
            wm.redraw_tag_menu();
        }
        return;
    }
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
