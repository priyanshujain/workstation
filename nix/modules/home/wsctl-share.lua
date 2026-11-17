local wsctl = "@wsctl@"
local share_label = "wsctl share"

local function open_share(mode)
  return function(ctx)
    local _, err = rex.session.new_layer{
      bounds = { 0.15, 0.15, 0.7, 0.6 },
      focus = true,
      layout = {
        block = {
          flavor = "com.superlogical.terminal.shell",
          label = share_label,
          options = {
            command = { wsctl, "share", "--session", ctx.session_id, "--block", ctx.block_id, "--mode", mode },
            exit = { on_completion = false },
          },
        },
      },
    }
    if err ~= nil then error(err, 0) end
  end
end

local function holds(layer, label)
  for _, block in ipairs(layer.blocks or {}) do
    if block.label == label then return true end
  end
  return false
end

local function holds_block(window, block_id)
  for _, layer in ipairs(window.layers or {}) do
    for _, block in ipairs(layer.blocks or {}) do
      if block.block_id == block_id then return true end
    end
  end
  return false
end

local function flash(message)
  rex.session.new_layer{
    bounds = { 0.35, 0.4, 0.3, 0.12 },
    focus = false,
    layout = {
      block = {
        flavor = "com.superlogical.terminal.shell",
        label = message,
        options = {
          command = { "/bin/sh", "-c", "printf '\\n  %s' \"$1\"; sleep 2", "sh", message },
          exit = { on_completion = true },
        },
      },
    },
  }
end

local function stop_sharing(ctx)
  local view, err = rex.session.view{}
  if err ~= nil then error(err, 0) end
  local stopped = 0
  for _, window in ipairs(view.windows or {}) do
    if holds_block(window, ctx.block_id) then
      for _, layer in ipairs(window.layers or {}) do
        if layer.kind ~= "tiled" and holds(layer, share_label) then
          rex.session.close_layer{ layer_id = layer.layer_id }
          stopped = stopped + 1
        end
      end
    end
  end
  if stopped > 0 then
    flash("Sharing stopped")
  else
    flash("This tab is not shared")
  end
end

rex.action{ name = "share_tab_view", title = "Share Tab (view)", category = "Share", run = open_share("view") }
rex.action{ name = "share_tab_input", title = "Share Tab (input)", category = "Share", run = open_share("input") }
rex.action{ name = "stop_sharing", title = "Stop Sharing", category = "Share", run = stop_sharing }
