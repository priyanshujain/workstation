local function open_pane(label, command, bounds)
  local _, err = rex.session.new_layer{
    bounds = bounds,
    focus = true,
    layout = {
      block = {
        flavor = "com.superlogical.terminal.shell",
        label = label,
        options = { command = command, exit = { on_completion = true } },
      },
    },
  }
  if err ~= nil then error(err, 0) end
end

local function share_tab(mode)
  return function(ctx)
    open_pane("Share Tab", { "wsctl", "share", "--session", ctx.session_id, "--block", ctx.block_id, "--mode", mode },
      { 0.2, 0.2, 0.6, 0.5 })
  end
end

local function stop_sharing(ctx)
  open_pane("Stop Sharing", { "/bin/sh", "-c", "wsctl share stop --block \"$1\"; sleep 3", "sh", ctx.block_id },
    { 0.3, 0.4, 0.4, 0.12 })
end

local function shared_tabs()
  open_pane("Shared Tabs", { "wsctl", "share", "list", "--wait" }, { 0.15, 0.15, 0.7, 0.6 })
end

rex.action{ name = "share_tab_view", title = "Share Tab (view)", category = "Share", run = share_tab("view") }
rex.action{ name = "share_tab_input", title = "Share Tab (input)", category = "Share", run = share_tab("input") }
rex.action{ name = "stop_sharing", title = "Stop Sharing", category = "Share", run = stop_sharing }
rex.action{ name = "shared_tabs", title = "Shared Tabs", category = "Share", run = shared_tabs }
