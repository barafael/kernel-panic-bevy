# Commands, controls and HUD parity audit

Paths: `KP/` = upstream/Kernel-Panic/, `ENG/` = upstream/RecoilEngine/rts/, `CONT/` = upstream/RecoilEngine/cont/, `RM/` = kernel-panic/src/; FEATURES.md, docs/ and spring-map/ are at the repo root. Every backticked quote after a `file:line` is verbatim from that line (a range means the quote is on one of those lines).

Baseline. KP's whitelist enables and disables widgets at load: KP/LuaUI/Widgets/kp_settings.lua:20 `or n=="CustomFormations2"`, :22 `or n=="HealthBars"`, :23 `or n=="HighlightSelectedUnits"`, :24 `or n=="Select n Center!"`, :35 `or n=="Kernel Panic autospam"`, :38 `or n=="Kernel Panic Default Commands"`, :39 `or n=="Kernel Panic Hotkeys"`. Everything else is disabled, including SmoothScroll, CameraShake, BuildETA, RelativeMinimap and TeamDiedMessages. KP plays with the TA Overhead camera: KP/LuaUI/Widgets/kp_settings.lua:67 `Spring.SetConfigInt("CamMode",1)` (first run: KP/LuaRules/Gadgets/LuaUI_force.lua:269). So the camera is compared with ENG/Game/Camera/OverheadController.cpp, not with the engine default (ENG/Game/CameraHandler.cpp:48 `.defaultValue(CCameraHandler::CAMERA_MODE_SPRING)`).

## Right-click default command, KP vs remake

The engine asks the selection's "leader" for a default command (ENG/Game/SelectedUnitsHandler.cpp:900 `int cmd = leaderUnit->commandAI->GetDefaultCmd(unit, feature);`). KP's widget may then override it (:901 `eventHandler.DefaultCommand(unit, feature, cmd);`). The answer is remembered on the press (ENG/Game/UI/GuiHandler.cpp:1245 `defaultCmdMemory = GetDefaultCommand(x, y);`) and given on the release (:1290 `SetActiveCommandIndex(defaultCmdMemory);`). CustomFormations2 only takes over when that default is Move.

| Selection | Under cursor | KP | Remake |
|---|---|---|---|
| any | ground | Move (drag = CF2 formation) | same |
| includes armed units | enemy unit | Attack; unarmed units refuse it and keep their orders | same |
| builders only (± factories) | enemy building | **Clear** (Spring's `CMD_RECLAIM`), which dismantles it (#7) | Move |
| mobile units | own or allied unit | **Guard** (#6) | Move |
| builder leads | damaged own unit | **Repair** (#6) | Move |
| factories only | own unit | Guard (as a rally order) | Move (rally) |
| builders + others | Bad Block feature | builders Clear (Spring's `CMD_RECLAIM`); the others refuse and keep their orders | builders Clear, the others walk there |
| includes Packets | own Port/Connection | Enter | same |
| includes Packets | enemy Port/Connection | Enter, which the gadget refuses: nothing happens (#27) | Attack |
| **Ports only** | anywhere | **Dispatch** to that point; Alt = until the buffer is empty (#1) | nothing |
| drag starting on an enemy | anywhere | **area attack** (#19) | single unit or ground attack at the release point |

## Findings (most important first)

### 1. Right-clicking with only Ports selected does nothing; KP dispatches Packets there — severity: high · confidence: verified
- **Original:**
  - KP/LuaUI/Widgets/kp_defaultcommands.lua:61-62 `if ud ~= port then` / `portonly = false`, then :65-66 `if portonly then` / `type = TYPE_DISPATCH`; :22 `[TYPE_DISPATCH] = CMD_DISPATCH,`; :71 `return cmd[type]`.
  - Dispatch is a map-point command, and the packets walk to the clicked point: KP/LuaRules/Gadgets/network_dispatch.lua:41 `type=CMDTYPE.ICON_MAP,`, :45 `cursor="Unload units",`, :60 `Spring.GiveOrderToUnit(nu, CMD.MOVE, {d.tx, d.ty, d.tz}, {})`.
  - With Alt it keeps going: :118 `return true, not opts.alt or bufferSize[team]==0`.
- **Remake:**
  - The right-click handler has no Port case. A Port-only click falls through to the plain move: RM/interaction/selection/right_click.rs:486 `let units = net.units(selected_q.iter().map(|(e, ..)| e));`, :489 `order: Order::Move(NetPos::quantize(target)),`.
  - RM/net/apply.rs:1075 `Order::Move(p) => QueuedCommand::Move(p.to_vec3()),` then gives that move to the immobile Port.
  - Dispatch is reachable only through D or the panel button.
- **In play:** select a Port and right-click the front. KP sends 12 Packets there (with Alt, the whole buffer). The remake does nothing.
- **Fix:** in the single-click branch, when every selected unit is a Port, issue `SimCommand::Cast { target: click, auto: alt }` and show the Unload cursor while hovering.

### 2. The minimap ignores the mouse, and clicks on it act on the world behind it — severity: high · confidence: verified
- **Original:** ENG/Game/UI/MiniMap.cpp:60 `CONFIG(bool, MiniMapFullProxy).defaultValue(true);`. In CMiniMap::MousePress:
  - Left starts a selection, :723 `selecting = true;`. The nearest own unit is picked by :653 `unit = CGameHelper::GetClosestFriendlyUnit(nullptr, pos, unitSelectRadius, gu->myAllyTeam);`, or a box is dragged.
  - Middle moves the camera there, :728 `if ((fullProxy && (button == SDL_BUTTON_MIDDLE)) || (!fullProxy && (button == SDL_BUTTON_RIGHT))) {`.
  - Right gives the default command at that map point, :739 `else if (fullProxy && (button == SDL_BUTTON_RIGHT)) {` → :741 `ProxyMousePress(x, y, button);`. CF2 draws formations on the minimap too.
- **Remake:**
  - The minimap plugin registers only its drawing systems (RM/ui/minimap.rs:41 `update_minimap.run_if(resource_exists::<MinimapState>),`), and the node is a bare image (:145 `ImageNode::new(image_handle.clone()),`).
  - No selection, right-click or camera system tests the minimap rectangle; they all ray-cast straight from the cursor.
- **In play:** right-clicking the minimap orders units to whatever is behind the top-left corner of the 3D view. Middle-clicking it pans the view. Left-clicking it deselects.
- **Fix:** add a minimap input system that consumes presses inside its rectangle: middle moves the camera, right gives the right-click order at the mapped point, left selects the nearest own unit or drags a box.

### 3. Homebases start with Repeat off; KP's autospam widget switches it on — severity: high · confidence: verified
- **Original:**
  - KP/LuaUI/Widgets/kp_autospam.lua:60 `if(unitTeam ~= Spring.GetMyTeamID()) then` limits it to the player's own team.
  - :63-64 `if (ud and ud.name == "kernel") then` / `Spring.GiveOrderToUnit(unitID, CMD.REPEAT, { 1 }, {})`; the hole and carrier get the same at :72-73 and :90-91.
  - It repeats this for the starting homebases at :117 `if f==2 then`, :130.
- **Remake:** RM/units/lifecycle/production.rs:270-271 `if kind.is_homebase() {` / `return Some(Producer::new());`, with the doc comment at :45 "homebases start with it off."
- **In play:** queue 5 Bits on the Kernel. KP cycles them forever and the button reads "Repeat on"; the remake builds 5 and stops.
- **Fix:** start the local human player's homebases with repeat on.

### 4. Health bars appear only on selected units; KP shows them on every damaged visible unit, enemies included, plus build bars — severity: high · confidence: verified
- **Original:**
  - It draws for all teams: KP/LuaUI/Widgets/unit_healthbars.lua:1059 `visibleUnits = GetVisibleUnits(-1,nil,false)`.
  - Health bars only when damaged: :39 `local drawFullHealthBars = false`, :633 `if (health) and ((drawFullHealthBars)or(hp<1)) and ((build==1)or(build-hp>=0.01)) then`.
  - A build-progress bar: :643 `AddBar("building",build,"build",(fullText and floor(build*100)..'%') or '')`.
  - Placed at :580 `height        = ud.height+14,`.
  - Also paralyze bars, and health/Clear bars on Bad Block features.
- **Remake:** RM/interaction/selection/health_bars.rs:65 `new_selections: Query<Entity, Added<Selected>>,` spawns bars on selection and removes them on deselection. They sit at a fixed :61 `const HEALTH_BAR_Y_OFFSET: f32 = 30.0;`, show at full health too, and there are no build, paralyze or feature bars.
- **In play:** you can't see how hurt enemies are, or how far an enemy nanoframe has got. Damaged own units show nothing unless selected. Tall models get the bar inside them.
- **Fix:** give every visible unit (any team) a bar when below 100% or under construction, at model height + 14, with KP's ramp; add build and paralyze bars.

### 5. Idle units, and units on Fight/Patrol, never leave their weapon range to engage — no Maneuver move state — severity: high · confidence: verified
- **Original:**
  - KP hides the Move-state button (KP/LuaUI/Widgets/hide_commands.lua:16 `[CMD.MOVE_STATE] = true,`), so units keep the default Maneuver: ENG/Sim/Units/UnitDef.cpp:465 `moveState = udTable.GetInt("moveState", (canmove && speed > 0.0f)? MOVESTATE_NONE: MOVESTATE_MANEUVER);`. Built units inherit it, ENG/Sim/Units/Unit.cpp:354 `moveState = builder->moveState;`.
  - An idle unit searches ENG/Sim/Units/CommandAI/MobileCAI.cpp:1208-1209 `float  extraRadius = 200.0f * owner->moveState * owner->moveState;` / `float searchRadius = owner->maxRange + extraRadius;`.
  - It gives itself a temporary attack, :1263 `c.SetTimeOut(gs->frameNum + GAME_SPEED * 5);`, leashed at :896 `const float maxTargetDist = (owner->moveType->GetManeuverLeash() * owner->moveState + owner->maxRange);`, with ENG/Sim/MoveTypes/MoveType.cpp:76 `maneuverLeash(500.0f),`.
  - On Fight legs: ENG/Sim/Units/CommandAI/MobileCAI.cpp:548 `const float leashRadius = 100.0f * owner->moveState * owner->moveState;`, then :554 `PushOrUpdateReturnFight();`.
- **Remake:**
  - Targets are searched within weapon range only: RM/units/combat/mod.rs:1122 `let scan_radius = aim::weapon_scan_radius(weapon_def, aim_from, map_gravity);`.
  - There is no move-state code anywhere. FEATURES.md:316-317 says "outside the range," / "no engagement."
- **In play:**
  - An idle Bit (Line range 256) in KP charges an enemy that comes within 456 elmos and chases it up to about 756 elmos from where it started.
  - A Fight order swerves up to about 356 elmos off its line and then returns to it.
  - In the remake, units stand and get shot by anything slightly out of range, and Fight walks past.
- **Fix:** port `GenerateAttackCmd` (temporary attack, 5 s timeout, leash 500·moveState + range) and `ExecuteFight`'s search around the closest point on the line, with the return to the line.

### 6. Right-clicking an own or allied unit moves onto it; KP guards it (builders repair it if damaged), and the remake's cursor says "Repair" — severity: high · confidence: verified
- **Original:**
  - CF2 steps aside unless the default is Move (KP/LuaUI/Widgets/unit_customformations2.lua:340 `if not (inButtonEvent or (defid == CMD_MOVE)) then return false end`), so the engine default applies:
    - ENG/Sim/Units/CommandAI/MobileCAI.cpp:993-994 `if (owner->unitDef->canGuard)` / `return CMD_GUARD;`;
    - ENG/Sim/Units/CommandAI/FactoryCAI.cpp:453 `return CMD_GUARD;`;
    - ENG/Sim/Units/CommandAI/BuilderCAI.cpp:1359-1360 `if (canRepairPointed)` / `return CMD_REPAIR;`, else guard (:1364-1365).
  - No KP FBI sets CanGuard, and the default is ENG/Sim/Units/UnitDef.cpp:384 `canGuard     = udTable.GetBool("canGuard",        true);`.
  - Repair applies only to damaged units (ENG/Sim/Units/UnitTypes/Builder.cpp:137 `if (u->health >= u->maxHealth)`). KP builders have repair by default: KP/units/assembler.fbi:53-54 `Builder=1;` / `WorkerTime=128;`, with UnitDef.cpp:395.
- **Remake:**
  - Anything that isn't a Packet, feature or enemy case falls to the plain move: RM/interaction/selection/right_click.rs:478 `// Plain ground move: every unit gets the clicked point, as a`, :485 `let target = drag_path.points[0];`.
  - The cursor claims otherwise: RM/interaction/cursor.rs:239-240 `} else if has_constructor {` / `request.set(CursorKind::Repair, 0);`.
- **In play:**
  - Right-click your Kernel with Bits: in KP they guard it (follow it and fight for it); in the remake they walk onto it and idle.
  - An Assembler over a damaged Socket shows Repair and then just walks there.
- **Fix:** an allied unit under the cursor gives `Order::Guard(target)` (factories take it as a rally guard), with the Defend cursor (ENG/Game/UI/MouseHandler.cpp:193 `AssignMouseCursor("Guard",        "cursordefend",     mCenter,  false);`). Show the Repair cursor only once Repair exists.

### 7. Builders can't dismantle buildings: KP's Clear (Spring's Reclaim) also takes down enemy (and own) buildings; the remake's Clear only removes features — severity: high · confidence: verified
- **Original, the default command:** a weaponless builder over an enemy building gets Reclaim. ENG/Sim/Units/CommandAI/BuilderCAI.cpp:1348 `if (owner->unitDef->canAttack && (owner->maxRange > 0.0f))` …, then :1351-1352 `if (owner->unitDef->canReclaim && pointed->unitDef->reclaimable)` / `return CMD_RECLAIM;`.
- **Original, the defaults behind it:**
  - ENG/Sim/Units/UnitDef.cpp:376 `reclaimable  = udTable.GetBool("reclaimable",  true);`
  - :396 `canReclaim   = udTable.GetBool("canReclaim",   builder) && (  reclaimSpeed > 0.0f);`
  - :368 `reclaimSpeed   = udTable.GetFloat("reclaimSpeed",   buildSpeed);`
  - ENG/Sim/Misc/ModInfo.cpp:81 `reclaimAllowEnemies            = true;`; KP's gamedata/modrules.tdf has no [RECLAIM] section.
  - KP made mobile units non-reclaimable (KP/units/bit.fbi:47 `Reclaimable=0;`, and the same in every mobile unit's FBI), but not buildings: socket.fbi and kernel.fbi have no such tag.
- **Original, how it resolves:**
  - The building's health drains to zero: ENG/Sim/Units/Unit.cpp:2056 `const float step = std::max(amount / buildTime, -buildProgress);`, :2060 `const float healthStep        = modInfo.reclaimUnitDrainHealth ? maxHealth * step : 0;`.
  - KP's own gadget expects unit targets: KP/LuaRules/Gadgets/network_arceffect.lua:55 `if c[1].id == CMD.RECLAIM then` → :59 `tx,ty,tz = Spring.GetUnitPosition(c[1].params[1])`.
  - No resources are involved.
- **Remake:**
  - Features only: RM/interaction/selection/right_click.rs:408 `&& let Some(feature) = crate::units::features::clearable_feature_at(`, and RM/interaction/ability.rs:678 `let Some(feature) = crate::units::features::clearable_feature_at(release.at, features.iter())`.
  - The code's own comments say so: RM/ui/hud/command_panel/commands.rs:21 "The builders' Reclaim is here as Clear (Kernel Panic has no", and RM/units/combat/lifecycle.rs:123 "other reset, reclaim at Unit.cpp:2068, has no Kernel Panic".
- **In play:**
  - In KP, one Assembler (KP/units/assembler.fbi:54 `WorkerTime=128;`) dismantles an enemy Socket (KP/units/socket.fbi:10 `BuildTime=3840;`) in 30 s, or a Kernel (KP/units/kernel.fbi:10 `BuildTime=3200;`) in 25 s, whatever their 20000/40000 HP, from up to KP/units/assembler.fbi:55 `BuildDistance=384;` away. That frees enemy datavents.
  - In the remake, a builder right-click on an enemy building walks there, and E plus a click on it does nothing.
- **Fix:** let Clear (or a new Dismantle order) target reclaimable units, draining maxHealth·WorkerTime/BuildTime per second and removing the building silently at 0. Make it the builders' right-click on enemy buildings.

### 8. A Shift-queued order mode stays armed after Shift is released — severity: medium · confidence: verified
- **Original:** a Shift commit sets `needShift` (ENG/Game/UI/GuiHandler.cpp:2074 `if ((button == SDL_BUTTON_LEFT) && (KeyInput::GetKeyModState(KMOD_SHIFT) || invertQueueKey)) {`), and the command is dropped as soon as Shift is up (:1051 `if (!invertQueueKey && (needShift && !KeyInput::GetKeyModState(KMOD_SHIFT))) {`). CF2 does the same for its commands: KP/LuaUI/Widgets/unit_customformations2.lua:640 `if (key == keyShift) and endShift then` … :642 `spSetActiveCommand(-1)`.
- **Remake:** RM/interaction/ability.rs:120-122 `/// After a committing click: Shift keeps the mode armed.` … `if !shift_held(keys) {`. Nothing disarms when Shift is released; only placement does (RM/ui/hud/placement.rs:408 `if state.need_shift && !mods(&keys).shift {`).
- **In play:** Shift-click three fight waypoints, release Shift, then click a unit to select it. KP selects the unit. The remake issues an unqueued fight order there, wiping the queue you just built.
- **Fix:** give `OrderCursorModes` a `need_shift` and disarm the frame Shift is released after a Shift commit.

### 9. Formation and group-move modifiers are missing — severity: medium · confidence: verified
- **Original:**
  - CF2 also draws formations on a left-drag with Move, Fight or Patrol armed, and on an Alt+left-drag with Attack armed: KP/LuaUI/Widgets/unit_customformations2.lua:328 `inButtonEvent = (activeid) and (button == 1) and ((activeid == CMD_PATROL) or` … :332 `(alt and ((activeid == CMD_ATTACK) or (activeid == CMD_UNLOADUNIT)))`.
  - It counts every canMove unit (:231 `return (((cmdID == CMD_MOVE) or (cmdID == CMD_FIGHT) or (cmdID == CMD_PATROL)) and uDefs[uDefID].canMove) or`), and KP factories are canMove: KP/units/kernel.fbi:29-30 `CanMove=1;` / `MaxVelocity=0;`.
  - Single clicks keep Ctrl and Alt, which the engine turns into group moves:
    - ENG/Game/SelectedUnitsAI.cpp:142 `//   CTRL:      Group Locked/Speed command  (maintain relative positions)`;
    - :211 `if ((c.GetOpts() & CONTROL_KEY) && ((cmdID == CMD_MOVE) || (cmdID == CMD_PATROL) || (cmdID == CMD_FIGHT))) {`;
    - :172 `if ((cmdID == CMD_MOVE) && (c.GetOpts() & ALT_KEY)) {` (a square front).
- **Remake:**
  - Armed modes fire on the press at a single point (RM/interaction/ability.rs:401 `if !modes.is(mode) || !mouse.just_pressed(MouseButton::Left) {`).
  - Formations skip factories (RM/interaction/selection/right_click.rs:298 `.filter(|(_, _, _, stats)| stats.speed > 0.0)`).
  - Right-click reads only Shift (:305).
- **In play:**
  - Press F and drag a line: KP spreads a fight line, while the remake sends everyone to the press point.
  - Ctrl+right-click: KP keeps the group's shape at the slowest unit's speed; the remake converges on one point.
- **Fix:** run the CF2 drag path for armed Move/Fight/Patrol (and Alt+Attack), count rally-taking factories, and port the Ctrl offset + group speed and the Alt square front.

### 10. Ability hotkeys: D fires at once instead of arming, A doesn't reach SIGTERM/Firewall/Obelisk, D packs Exploits up — severity: medium · confidence: verified
- **Original:**
  - The bindings: KP/LuaUI/Widgets/kp_hotkeys.lua:55 `Spring.SendCommands({"bind d dgun"})` and :63 `Spring.SendCommands({"bind d dispatch"})`.
  - A key picks the first command whose action matches (ENG/Game/UI/GuiHandler.cpp:1920 `if ((static_cast<int>(a) != iconCmd) && (cmdDesc.action != action.command))`), and a targeted command only arms (:2026 `newInCommand = a;`).
  - NX Flag uses KP/LuaRules/Gadgets/specialattack.lua:29 `action = "dgun",`.
  - SIGTERM and Firewall sit on A: KP/LuaRules/Gadgets/airstrike.lua:30 and KP/LuaRules/Gadgets/network_reflectorshield.lua:28 `action = "attack",`, with ENG/Game/UI/KeyBindings.cpp:156 `{            "a", "attack"       },`. The Obelisk fires through the plain Attack command.
  - On an Exploit, D matches nothing; U undeploys it (KP/LuaUI/Widgets/kp_hotkeys.lua:59 `Spring.SendCommands({"bind u undeploy"})`).
- **Remake:**
  - RM/interaction/ability.rs:442 "casts the selection's aimed abilities (command-fire, Dispatch)" runs immediately (:455 `if !keys.just_pressed(KeyCode::KeyD) || ctrl_held(&keys) {`).
  - A sends only RM/ui/hud/command_panel/activation.rs:123 `(KeyCode::KeyA, CmdId::Attack),`. Terminal, Firewall and Obelisk don't list Attack, so :208 `(None, _) => continue,`.
  - The SIGTERM button advertises "d" (RM/ui/hud/command_panel/commands.rs:455).
  - D also packs Exploits up (ability.rs:414-440).
- **In play:** a KP player presses D on a Pointer, then clicks the target. The remake has already fired the NX Flag wherever the cursor was, and a Port has already spent 12 packets there. A on a Terminal does nothing.
- **Fix:** make D arm `Mode::Ability` so the click supplies the target; route A to the SIGTERM, Firewall and Obelisk casts; drop D-undeploy.

### 11. Enter (E or the button) absorbs only packets already next to a teleporter; KP's Enter is a targeted order — severity: medium · confidence: verified
- **Original:** KP/LuaRules/Gadgets/network_enter.lua:30 `type=CMDTYPE.ICON_UNIT,`, :33 `action="enter",` (CommandFallback :74-90 walks the packets there), bound by KP/LuaUI/Widgets/kp_hotkeys.lua:64 `Spring.SendCommands({"bind e enter"})`. E arms it, you click your Port, and the packets walk there and enter.
- **Remake:**
  - Enter is an immediate command (RM/ui/hud/command_panel/commands.rs:487-488 `CmdId::Enter,` / `Icon,`), and RM/ui/hud/command_panel/activation.rs:326 `net.issue(SimCommand::Enter { units });`.
  - Then RM/units/mechanics/network_buffer.rs:300 `// Nearest friendly teleporter within range.` and :306 `if nearest.is_some_and(|d_sq| d_sq <= enter_sq) {`, with :39 `pub const ENTER_DISTANCE: f32 = 150.0;`.
- **In play:** with packets spread over the map, press E and click the Port. KP brings them all home. The remake absorbs only those within 150 elmos, and the click selects the Port.
- **Fix:** make Enter a targeted mode that issues `Order::Enter(target)`; the right-click path already walks the packets there.

### 12. Guards never retaliate against whoever hits the guarded unit — severity: medium · confidence: verified
- **Original:** ENG/Sim/Units/CommandAI/MobileCAI.cpp:627 `constexpr int retaliationTimeout = 40;`, :630 `(guardee->lastAttackFrame + retaliationTimeout > gs->frameNum) &&`, :634 `commandQue.push_front(Command(CMD_ATTACK, c.GetOpts(), guardee->lastAttacker->id));`.
- **Remake:** RM/interaction/movement.rs:729 `/// let the auto-attack path engage anything in range. When the target`, with :62 `const GUARD_DISTANCE: f32 = 48.0;`. The guard only trails.
- **In play:** Bits guarding a Kernel that Pointers shell from 1400 elmos charge the Pointers in KP; in the remake they stay by the Kernel.
- **Fix:** record each unit's last attacker and frame; a guard pushes an attack on that attacker within 40 frames.

### 13. Control groups: no group 0, a unit can be in several groups, no Ctrl+Shift add, Alt toggle or double-tap focus — severity: medium · confidence: verified
- **Original:**
  - ENG/Game/UI/KeyBindings.cpp:138 `{ "Any+0", "group0" },`.
  - ENG/Game/UI/Groups/GroupHandler.cpp:84 `if (KeyInput::GetKeyModState(KMOD_CTRL))` and :87 `if(!KeyInput::GetKeyModState(KMOD_SHIFT))`: Ctrl+Shift adds to the group instead of replacing it.
  - :113 `else if (KeyInput::GetKeyModState(KMOD_ALT))` toggles the group's members in the selection.
  - :136 `if (selectedUnitsHandler.IsGroupSelected(num)) {` → :138 `camHandler->GetCurrentController().SetPos(group->CalculateCenter());` focuses the camera on a second press.
  - A unit has only one group: ENG/Sim/Units/Unit.cpp:1932 `group->RemoveUnit(this);`.
- **Remake:** RM/interaction/selection/groups.rs:26 `pub const GROUP_COUNT: usize = 9;`. :100-101 say "is intentionally unbound — Spring" / "has no group 0;". :70-72 `if ctrl {` … `groups.assign(slot, live);` ignores Shift, and the slots are independent.
- **In play:** Ctrl+0 and 0 do nothing. Ctrl+2 on a unit from group 1 leaves it in both groups. Ctrl+Shift+1 replaces group 1. Pressing 1 twice doesn't move the camera.
- **Fix:** 10 slots; take a unit out of its old slot when assigning; Ctrl+Shift adds; Alt toggles; a second press focuses the camera.

### 14. No "select all of this type" across the map (Ctrl+Z, Ctrl+double-click), and no Ctrl+B / Ctrl+W — severity: medium · confidence: verified
- **Original:**
  - ENG/Game/UI/KeyBindings.cpp:256 `"select AllMap+_InPrevSel+_ClearSelection_SelectAll+"` (Ctrl+z);
  - :250 `"select AllMap+_Builder_Idle+_ClearSelection_SelectOne+"` (Ctrl+b: cycles idle builders and jumps the camera);
  - :254 `"select AllMap+_Not_Aircraft_Weapons+_ClearSelection_SelectAll+"` (Ctrl+w);
  - double-click: ENG/Game/SelectedUnitsHandler.cpp:363 `// double click, select all units of same type (on screen, unless CTRL is pressed)`, :380 `if (!doInViewTest || KeyInput::GetKeyModState(KMOD_CTRL) || camera->InView((u)->midPos))`.
- **Remake:**
  - Only Ctrl+A, and a remake-only Q documented as RM/interaction/selection/core.rs:417 `(Spring's "select all of the same type in view")`. Spring's plain q is an obsolete group command, ENG/Game/UI/KeyBindings.cpp:183 `{            "q", "groupselect"  },`.
  - Ctrl+click toggles and never reaches the double-click branch (RM/interaction/selection/core.rs:353 `} else if ctrl {`).
- **In play:** KP players grab every Bit on the map with Ctrl+Z or Ctrl+double-click; in the remake only Ctrl+A comes close.
- **Fix:** Ctrl+Z (types in the selection, whole map); Ctrl+double-click over the whole map; Ctrl+B cycles idle builders with a camera jump.

### 15. Game start: the camera doesn't go to your base and the base isn't selected — severity: medium · confidence: verified
- **Original:**
  - KP/LuaUI/Widgets/gui_center_n_select.lua:27 `Spring.SetCameraTarget(x, y, z)` targets the start position on load.
  - At game start, if the team has exactly one unit: :41 `Spring.SelectUnitArray{unitArray[1]}` and :45 `Spring.SetCameraTarget(x, y, z)`.
- **Remake:** RM/map_loading/mod.rs:1125 `let focus = Vec3::new(world_w / 2.0, center_height, world_d / 2.0);` puts the camera on the map centre, and nothing is selected.
- **In play:** on maps with many starts your base can be off-screen at the start, and nothing is selected.
- **Fix:** on the first sim frame, snap the focus to the local base and select it.

### 16. No screen-edge scrolling — severity: medium · confidence: verified
- **Original:** ENG/Game/CameraHandler.cpp:60 `CONFIG(bool, WindowedEdgeMove).defaultValue(true)` (and FullscreenEdgeMove at :61), with a band of ENG/Game/Camera.cpp:21 `.defaultValue(0.02f)` (EdgeMoveWidth).
- **Remake:** RM/rendering/camera.rs:261 `// --- Pan (arrow keys only — WASD reserved for hotkeys) ---`. There is no edge code.
- **Fix:** pan inside a 2% border band at arrow-key speed, fading towards the inner edge.

### 17. Middle-drag scrolls the opposite way, at half speed — severity: medium · confidence: verified
- **Original:**
  - ENG/Game/UI/MouseHandler.cpp:299 `camHandler->GetCurrentController().MouseMove(float3(dx, dy, invertMouse ? -1.0f : 1.0f));` → ENG/Game/Camera/OverheadController.cpp:104 `pos.x += (move.x * pixelSize * (1 + moveFast * camera->moveFastMult * fastScale) * scrollSpeed);`.
  - With :213 `pixelSize = (camera->GetTanHalfFov() * 2.0f) / globalRendering->viewSizeY * height * 2.0f;`, the focus moves *with* the mouse, two pixels of view per mouse pixel.
- **Remake:** RM/rendering/camera.rs:334 `let new_focus = bounds.clamp_focus(state.focus + (*anchor - current));` is a grab-pan: the ground follows the cursor 1:1, so the camera moves against the mouse.
- **Fix:** move the focus by (dx, dy) × 2 × the world size of a pixel, along screen right/down.

### 18. Zoom step, limits and anchor — severity: medium · confidence: verified
- **Original:**
  - ENG/Game/UI/MouseHandler.cpp:66 `CONFIG(float, ScrollWheelSpeed).defaultValue(-25.0f)` and ENG/Game/Camera/OverheadController.cpp:128 `const float altZoomDist = height * move * 0.007f * shiftSpeed;` give 17.5% per notch.
  - Zoom in goes toward the cursor (:142 `// ZOOM IN to mouse cursor instead of mid screen`); zoom out goes from mid-screen (:174 `// ZOOM OUT from mid screen`).
  - Minimum :25 `CONFIG(float, OverheadMinZoomDistance).defaultValue(60.0f)`; maximum :63 `maxHeight = 9.5f * std::max(mapDims.mapx, mapDims.mapy) * configHandler->GetFloat("OverheadMaxHeightFactor");`.
- **Remake:**
  - RM/rendering/camera.rs:300 `let factor = 1.0 - zoom_ticks.det_clamp(-5.0, 5.0) * 0.06;`, keeping the cursor point fixed both ways.
  - Limits :104 `#[default(150.0)]` and :106 `#[default(3000.0)]`.
- **In play:**
  - It takes about 3× the notches.
  - You can't zoom out over a whole large map (e.g. KP allows 9728 on a 16×16 map).
  - On maps wider than 6000 elmos, the start distance (half the map) exceeds 3000, so the first notch jumps.
- **Fix:** ×(1∓0.175) per notch, min 60, max 9.5 × map squares, zoom out about the screen centre.

### 19. A right-drag that starts on an enemy is an area attack in KP; the remake attacks the unit or ground under the release — severity: medium · confidence: verified
- **Original:**
  - ENG/Game/UI/GuiHandler.h:234 `bool newAttackMode = true;` (also KP/LuaUI/Widgets/KP_CtrlPanel.txt:6 `newAttackMode     1  enable attack by area selection`) makes Attack ENG/Game/UI/GuiHandler.cpp:1010 `cd.type = CMDTYPE_ICON_UNIT_OR_AREA;`.
  - Past :2356 `if (mouse->buttons[button].movement <= mouse->dragCircleCommandThreshold) {` it becomes :2383 `// create circular area-command`, with :2405 `c.PushParam(std::min(maxRadius, innerPos.distance2D(cameraPos + mouseDir * outerDist)));`, centred on the press point.
- **Remake:** RM/interaction/selection/right_click.rs:307 `if attack_drag && drag_path.points.len() > 1 {`, :308 `// The remembered Attack resolves at the release`, giving AttackUnit or AttackGround at the release point. The comment on :309 names the type as unit-or-map, which is not the type KP uses.
- **In play:** right-drag from an enemy across a blob. KP spreads attacks over everything in the disc; the remake attacks one unit or one ground point.
- **Fix:** send the right-drag through the same `AttackArea` path as the armed A-drag (centre at the press point, radius from the drag).

### 20. Alt-Dispatch drains the buffer 15× too fast and stacks the packets — severity: medium · confidence: verified
- **Original:**
  - KP/LuaRules/Gadgets/network_dispatch.lua:95 `while index < 12 and bufferSize[team] > 0 do`.
  - Blocked slots are skipped without using a packet: :101 `if not Spring.GetGroundBlocked(tx-8, tz-8, tx+8, tz+8) and (blocking_p==2 or blocking_b==2) and feature==nil then`.
  - It runs again on each slow update (ENG/Sim/Units/Unit.cpp:1059 `commandAI->SlowUpdate();`, ENG/Sim/Misc/GlobalConstants.h:60 `static constexpr int UNIT_SLOWUPDATE_RATE = 15;`), so at most 24 packets/s.
- **Remake:**
  - RM/units/mechanics/network_buffer.rs:215 `pub fn tick_auto_dispatch(` re-fires every tick (:213 "peeling 12 Packets per").
  - :252 `let available = buffer.take(team.0, DISPATCH_MAX as u32) as usize;` spawns them on a fixed ring (:262 `ops::cos(angle) * DISPATCH_RING_RADIUS,`) with no blocking test.
- **In play:** Alt-dispatching 100 packets takes about 4 s in KP and fills free spots. The remake dumps them in 9 ticks onto 12 stacked points.
- **Fix:** re-fire every 15 ticks; test each slot and skip blocked ones without taking a packet.

### 21. Self-destruct: Stop cancels it and a second Ctrl+D restarts it — KP is the other way round — severity: medium · confidence: verified
- **Original:** ENG/Sim/Units/CommandAI/CommandAI.cpp:968-969 `if (owner->selfDCountdown != 0) {` / `owner->selfDCountdown = 0;` and :971 `owner->selfDCountdown = owner->unitDef->selfDCountdown*2+1;` make it a toggle. ExecuteStop (:1531-1541) never touches it.
- **Remake:**
  - RM/net/apply.rs:693 `SimCommand::Stop { .. } => {` … :696 `.remove::<SelfDestructCountdown>();`.
  - :759 `SimCommand::SelfDestruct { .. } => {` … :763 `self.commands.entity(own.e).insert(countdown);` starts a fresh countdown every time.
  - FEATURES.md:129 says "5-second countdown (cancelled by".
- **In play:** in KP a second Ctrl+D saves the unit and S doesn't. In the remake a second Ctrl+D restarts the timer and S saves it.
- **Fix:** make SelfDestruct toggle the countdown, and leave the countdown alone on Stop.

### 22. KP's gadget commands can't be Shift-queued (Deploy, Launch Mines, NX Flag, Dispatch, SIGTERM, Firewall) — severity: medium · confidence: verified
- **Original:** button commands carry the modifiers (ENG/Game/UI/GuiHandler.cpp:1345 `c.SetOpts(CreateOptions(rightMouseButton));`). KP's own tooltip says so: KP/LuaRules/Gadgets/specialattack.lua:56 "Tip: The button is queuable, the hotkey is not.".
- **Remake:** RM/net/mod.rs:350-351 `Deploy {` / `units: Units,` and :359-362 `Cast {` … `auto: bool,` have no queue flag. The same tip is still shown (RM/ui/hud/command_panel/commands.rs:428).
- **In play:** Shift-move a Bug, then Shift-click Deploy. KP deploys on arrival; the remake deploys on the spot.
- **Fix:** add a queue flag to Deploy and Cast and run them from the command queue.

### 23. DOS paralysis has no visual — severity: medium · confidence: verified
- **Original:**
  - KP/weapons/corruptionweapons.tdf:113-114 `paralyzer=1;` / `paralyzetime=5;`.
  - The widget draws a paralyze bar, KP/LuaUI/Widgets/unit_healthbars.lua:683 `AddBar("paralyze",emp,empcolor_index,infotext)`, and a stunned overlay, :918 `glTexture("LuaUI/Images/paralyzed.png")`.
- **Remake:** RM/units/combat/lifecycle.rs:109 `pub struct Stunned {` and RM/units/combat/mod.rs:812 `pub struct StunCharge(pub f32);` are read by no HUD or rendering code.
- **In play:** a paralysed unit looks normal.
- **Fix:** add a paralyze bar with a blinking stunned state, and an overlay on stunned units.

### 24. Attack, guard and target lines follow enemies through the fog — severity: medium · confidence: verified
- **Original:** ENG/Rendering/CommandDrawer.cpp:31 `if ((cmdUnit->losStatus[caiOwner->allyteam] & (LOS_INLOS | LOS_INRADAR)) == 0)` drops the line.
- **Remake:** RM/interaction/movement.rs:1169 `&& let Ok(t_gtf) = targets.get(atk.target)` draws to the true position with no visibility test. Guard and forced-target lines do the same.
- **In play:** attack an enemy and let it slip out of sight: the line and ring keep showing where it is.
- **Fix:** skip unit-targeted lines whose target the local team hasn't spotted.

### 25. Shift-clicking an already queued point or target doesn't remove it — severity: low · confidence: verified
- **Original:** ENG/Sim/Units/CommandAI/CommandAI.cpp:1044 `if (CancelCommands(c, commandQue, first) > 0) {`, with :49 `static const float COMMAND_CANCEL_DIST = 17.0f;` and :1351 `if ((c1p - c2p).SqLength2D() >= (COMMAND_CANCEL_DIST * COMMAND_CANCEL_DIST))`.
- **Remake:** RM/interaction/selection/right_click.rs:565 `if enqueue && orders.busy(entity) {` → :583 `queue.push(cmd);` always appends.
- **Fix:** cancel a queued non-build order with the same ID within 17 elmos (or on the same target).

### 26. Selection highlight: team-tinted brightening plus a hover highlight; KP draws an additive health-coloured glow on selected units only — severity: low · confidence: verified
- **Original:** KP/LuaUI/Widgets/gui_hilight_selectedunits.lua:211 `health>maxHealth/2 and 2-2*health/maxHealth or 1, -- red`, :172 `glBlending(GL_SRC_ALPHA, GL_ONE)`, :223 `SetUnitColor(unitID, outline and 0.25 or 0.125)`. The engine boxes are turned off: :113 `f:write('unitBox  0 1 0 ' .. alpha)`.
- **Remake:** RM/interaction/selection/highlight.rs:29 `const HOVER_BRIGHTNESS: f32 = 1.5;`, :31 `const SELECTED_BRIGHTNESS: f32 = 2.5;`, :273 `const TINT_MIX: f32 = 0.4;`.
- **In play:** in KP, damaged selected units glow yellow or red, a health readout. In the remake they all flash team colour, and hovering any unit lights it up.
- **Fix:** an additive green→yellow→red overlay plus outline on selected units only.

### 27. Packets plus an enemy Port/Connection: KP's Enter is refused (nothing happens); the remake attacks — severity: low · confidence: verified
- **Original:**
  - KP/LuaUI/Widgets/kp_defaultcommands.lua:47 `if s == "unit" and teleporter[Spring.GetUnitDefID(t)] then` has no team test, so the default becomes Enter.
  - The gadget then refuses it for other teams: KP/LuaRules/Gadgets/network_enter.lua:65 `Spring.GetUnitTeam(param[1])==team`.
- **Remake:** RM/interaction/selection/right_click.rs:383 `&& unit_info_q.get(t).is_ok_and(|tt| Some(tt.0) == my_team)`, so an enemy teleporter falls through to the attack branch.
- **In play:** in KP, a selection containing Packets must use A to attack an enemy Port. This is a KP quirk; only matters for strict fidelity.

### 28. Selection mechanics: Ctrl+box doesn't deselect, 0.3 s double-click, 8 px drag threshold, box tested in screen space at the unit base — severity: low · confidence: verified
- **Original:**
  - Ctrl+box removes already-selected units: ENG/Game/SelectedUnitsHandler.cpp:321 `if (KeyInput::GetKeyModState(KMOD_CTRL) && (selectedUnits.find(u->id) != selectedUnits.end())) {`.
  - Units are tested at their mid position: :310 `const float4 vec(u->midPos, 1.0f);`.
  - ENG/Game/UI/MouseHandler.cpp:65 `CONFIG(float, DoubleClickTime).defaultValue(200.0f)` and :69 `CONFIG(int, MouseDragSelectionThreshold).defaultValue(4)`.
  - The threshold counts path length: :279 `buttons[SDL_BUTTON_LEFT ].movement += movedPixels;`.
  - The press corner is re-projected from its ground point (GetSelectionBoxCoeff).
- **Remake:**
  - RM/interaction/selection/core.rs:348 `commands.entity(entity).insert(Selected);` adds every own unit in the box, with or without Ctrl.
  - :86 `const DOUBLE_CLICK_INTERVAL: f32 = 0.30;` and :81 `const DRAG_THRESHOLD: f32 = 8.0;` (:264 `let distance = (current - start).length();`).
  - Units are tested at :339 `.world_to_viewport(camera_transform, global_transform.translation())`.
- **In play:** Ctrl+drag can't drop units from the selection. Panning or zooming mid-drag selects a different area than KP would.

### 29. The context cursor disagrees with what the right-click does — severity: low · confidence: verified
- **Original:** the cursor is the default command's: Guard over allies (#6), Reclaim over a Bad Block with builders (ENG/Game/UI/MouseHandler.cpp:197 `AssignMouseCursor("Reclaim",      "cursorreclamate",  mCenter,  false);`), and Move for unarmed units over enemies.
- **Remake:** shows Repair for constructors over any friendly unit (RM/interaction/cursor.rs:239-240), Normal for unarmed units over enemies (:235 `request.set(CursorKind::Normal, 0);`), and never considers features.
- **Fix:** derive the cursor from the same resolver as the right-click.

### 30. No build facing; a second numpad press disarms instead of setting the facing — severity: low · confidence: verified
- **Original:**
  - KP/LuaUI/Widgets/kp_hotkeys.lua:31 `Spring.SendCommands({"bind numpad2 buildfacing south"})` is bound before the buildunit actions. It is only processed while a build is armed (ENG/Game/UI/GuiHandler.cpp:1731 `// only process the build options while building`), so the second press sets the facing.
  - ENG/Game/UI/KeyBindings.cpp:149 `{       "[", "buildfacing inc"  },` and :153 `{   "Any+z", "buildspacing inc" },`.
  - KP/LuaUI/Widgets/kp_autospam.lua:158 `Spring.SendCommands({"buildfacing "..facing})` presets the facing toward the map centre from the homebase.
- **Remake:**
  - RM/ui/hud/command_panel/activation.rs:216 `let toggled_off = msg.hotkey && active_command(&modes, &placement) == Some(msg.id);` makes the second press disarm.
  - The facing comes from the vent (RM/units/lifecycle/construction.rs:721 `// A factory on a datavent fronts the vent's baked facing`).
- **In play:** numpad8 twice cancels the Socket placement instead of facing it north.

### 31. Command-queue lines: colours and widths differ, and Fight is drawn red like Attack — severity: low · confidence: verified
- **Original:** loaded at ENG/Game/Game.cpp:770 `cmdColors.LoadConfigFromFile("cmdcolors.txt");`, the colours are CONT/cmdcolors.txt:68 `move        0.5  1.0  0.5  0.4`, :69 `attack      1.0  0.2  0.2  0.4`, :70 `fight       0.5  0.5  1.0  0.4`, :71 `guard       0.3  0.3  1.0  0.4`, :67 `build       0.0  1.0  0.0  0.4`, :55 `queuedLineWidth   2.0`.
- **Remake:** RM/interaction/movement.rs:1087 `const FIGHT_COLOR: Color = Color::srgb(1.0, 0.3, 0.25);` is used for both (:1116 `QueuedCommand::AttackMove(_) => FIGHT_COLOR,`, :1117 `QueuedCommand::AttackUnit { .. } => FIGHT_COLOR,`). Also :1088 `const GUARD_COLOR: Color = Color::srgb(0.85, 0.95, 1.0);` and :1085 `const BUILD_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);`, opaque at 1 px.
- **Fix:** take the colours, alpha and width from cmdcolors.txt.

### 32. Formation drag preview and order markers — severity: low · confidence: verified
- **Original:** KP/LuaUI/Widgets/unit_customformations2.lua:671 `glLineStipple(2, 4095)` draws the dragged line in the command colour (:217 `if (cmdID == CMD_MOVE) then glColor(0.5, 1.0, 0.5, alpha) -- Green`), fading out after release. There are no node dots.
- **Remake:** RM/interaction/selection/right_click.rs:685 `base_color: Color::srgba(1.0, 1.0, 0.3, 0.4),` puts yellow tori on the nodes, with no line. The 1.5 s tori at every order target (:625 `lifetime: Timer::from_seconds(1.5, TimerMode::Once),`) are a remake addition.

### 33. The geo highlight doesn't respond to the metal view (F4) — severity: low · confidence: verified
- **Original:** KP/LuaUI/Widgets/kp_geoshighlight.lua:89 `if isPlayerPlacingBuilding or Spring.GetMapDrawMode()=="metal" then`, with ENG/Game/UI/KeyBindings.cpp:211 `{ "Any+f4",  "ShowMetalMap"          },`.
- **Remake:** RM/ui/hud/geo_highlight.rs:60 `let placing = placement.kind.is_some_and(|k| k.is_small_building());` only; there is no F4.

### 34. Camera details — severity: low · confidence: verified
- **Pitch:** ENG/Game/Camera/OverheadController.h:52 `static constexpr float DEFAULT_ANGLE = 0.464f;` (63.4° below horizontal) vs RM/rendering/camera.rs:39 `let pitch = std::f32::consts::FRAC_PI_4;`.
- **Map-edge clamp:** ENG/Game/Camera/OverheadController.cpp:205 `pos.x = std::clamp(pos.x, 0.01f, mapDims.mapx * SQUARE_SIZE - 0.01f);` vs RM/rendering/camera.rs:78 `margin_fraction: 0.25,`.
- **Arrow keys:** ENG/Game/Camera/OverheadController.cpp:81 `move *= (math::sqrt(move.z) * 200.0f);` is screen-relative, and Shift/Ctrl speed it up or slow it down (ENG/Game/UI/KeyBindings.cpp:242 `{    "Any+shift",    "movefast"     },`, :241 `{    "Any+ctrl",     "moveslow"     },`; Ctrl+wheel tilts, :244 `{    "Any+ctrl",     "movetilt"     },`). The remake uses RM/rendering/camera.rs:280 `let speed = settings.pan_speed * (state.distance / 500.0).det_max(0.3);` with no modifiers.
- **Build-bar middle-click:** KP/LuaUI/Widgets/kp_buildbar.lua:831 `if button==2 then` → :833 `Spring.SetCameraTarget(x,y,z)`; the remake handles only RM/ui/hud/build_bar.rs:274 `for button in [MouseButton::Left, MouseButton::Right] {`.
- **Engine camera keys not ported:** ENG/Game/UI/KeyBindings.cpp:75 `{         "Any+tab", "toggleoverview" },`, :238 `{    "Any+pageup",   "moveup"       },`, :200 `{   "Any+t", "track" },`. The remake uses T for SetTarget.

### 35. Minimap drawing — severity: low · confidence: verified
- **Original:** ENG/Game/UI/MiniMap.cpp:59 `CONFIG(std::string, MiniMapGeometry).defaultValue("2 2 200 200");`, :71 `CONFIG(int, MiniMapDrawCommands).defaultValue(1)`, :73 `CONFIG(bool, MiniMapDrawProjectiles).defaultValue(true)`. Units are drawn as per-type icons from ICONTYPES.TDF.
- **Remake:** RM/ui/minimap.rs:201 `// Unit dots.`: 3×3 team-colour dots, with no selection, command or projectile drawing; the long side is 200 px.

### 36. Hover tooltip is missing KP's state and combat lines — severity: low · confidence: verified
- **Original:** KP/LuaUI/Widgets/kp_tooltip.lua:212 (`"Build progress: "`), :216 (`paralysed`), :219 (`cloacked`), :234 (`Damage: `), :250 (`    XP: `), :261 (current/max `Speed: `).
- **Remake:** RM/ui/hud/tooltip.rs:282 `("Health: ", HEALTH_LABEL),` plus name, Bufferised Packets and max speed only.

### 37. SIGTERM/Firewall countdown label starts at 96 s; KP shows 90 — severity: low · confidence: verified
- **Original:** KP/LuaRules/Gadgets/airstrike.lua:68 `text = math.ceil((t - time) / 32) .. "s"` counts 32-frame "seconds".
- **Remake:** RM/ui/hud/command_panel/commands.rs:262 `format!("{}s", seconds.ceil().det_max(1.0) as u32)` counts real seconds of RM/units/mechanics/command_fire.rs:195 `pub const LUA_ABILITY_RELOAD: f32 = 90.0 * 32.0 / 30.0;`.
- **Fix:** show ceil(remaining·30/32).

### 38. Build bar, command panel and player list details — severity: low · confidence: verified
- **Build-bar icon tooltip:** RM/ui/hud/build_bar.rs:653 `kind.unitname().to_string()` shows "hole" and "carrier" where KP shows the human name.
- **Command panel:** KP's LuaUI layout reserves the last row for Prev / page / Next (CONT/LuaUI/layout.lua:127 `local firstSpecial = (xIcons * (yIcons - 1))`, active by :225 `LayoutButtons = DefaultHandler`), so a page holds 24 commands. The remake fits 25 plus arrows in the last two slots (RM/ui/hud/command_panel/layout.rs:80-81 `pub fn prev_slot(&self) -> usize {` / `self.icons_per_page() - 2`).
- **Player list:** AdvPlayersList is whitelisted and shows in every game. The remake's list appears only in networked matches (RM/ui/hud/net_overlay.rs:354 `let rows = match status.filter(|s| s.networked && shown.0) {`).

### 39. Attack and Enter order details — severity: low · confidence: verified
- **Armed buildings drop attack orders:** the remake drops the order on an out-of-range target after RM/units/combat/mod.rs:102 `pub const TARGET_LOST_FRAMES: u64 = 64;` (:101 "Four slow updates of 16 frames"; a slow update is 15). The engine only counts down while the target is off radar (ENG/Sim/Units/CommandAI/CommandAI.cpp:1482 `if ((targetUnit->losStatus[owner->allyteam] & LOS_INRADAR))`).
- **OnlyTargetCategory:** attack orders are refused by OnlyTargetCategory (RM/net/apply.rs:1093 `units.retain(|u| self.can_hit(u.kind, t_kind));`). The engine's AllowedCommand checks only IsAttackCapable and that the target exists.
- **Spawn-stunned packets:** a packet still spawn-stunned when it reaches the Port loses its Enter order (RM/units/mechanics/network_buffer.rs:90 `ec.remove::<EnterOrder>();`, then :297 `if packet_unit.0 != UnitKind::Packet || stun.is_some() {`). KP keeps the order and enters later (KP/LuaRules/Gadgets/network_enter.lua:80 `if dist < enterDist and (not spawnStun[u] or spawnStun[u] < Spring.GetGameFrame()) then`).

## Deliberate or documented deviations (one line each)
- **Builder Repair, Wait and mobile Repeat are omitted** (RM/ui/hud/command_panel/commands.rs:17-21; FEATURES.md:200-201). Questionable: KP builders repair by default at maxHealth·128/BuildTime per second, i.e. 667 HP/s on a Socket and 1600 HP/s on a Kernel, free of cost (modrules [REPAIR] energyCostFactor=0). It is also KP's right-click on a damaged ally.
- **D on a Bug is Deploy, not Bombard** (RM/ui/hud/command_panel/commands.rs:414-416, documented).
- **T/X set/unset target, Q select type in view, R or Alt+middle orbit** are remake extras; Spring uses T for track, X for onoff and q for groupselect.
- **Camera smoothing at 8/s** (RM/rendering/camera.rs:115-118); KP's SmoothScroll widget is off.
- **Group double-tap camera focus** is a documented TODO (RM/interaction/selection/groups.rs:5-6).
- **Minifac autospam on every team** (FEATURES.md:432-437); kp_autospam only acts for the local player.

## Doc drift
- **docs/kernel-panic-game-design.md:388** says "with only Ports selected, right-click = **Dispatch**". The code doesn't (#1).
- **docs/kernel-panic-game-design.md:382** says "your home base starts with *repeat* on". The code doesn't (#3).
- **FEATURES.md:100** says "Shift + left-click to add or remove units". In the code Shift adds and Ctrl toggles, as in Spring.
- **FEATURES.md:89-92** says middle-drag orbits and Q/E rotate the camera. The code pans with middle-drag; Q selects, E is Enter/Clear.
- **FEATURES.md:82** says "the camera can't leave" the map's bounds. It has a 25% margin.
- **Factory rally orders:** FEATURES.md:200-201 and RM/ui/hud/command_panel/commands.rs:17-20 say they aren't simulated. They are listed and simulated: RM/units/content/definitions.rs:255 `pub fn takes_rally_orders(self) -> bool {`, RM/net/apply.rs:608 `fn give_factory(&mut self, e: Entity, cmd: QueuedCommand, queue: bool) {`.
- **Reclaim:** FEATURES.md:203-204, RM/ui/hud/command_panel/commands.rs:21-22 and RM/units/combat/lifecycle.rs:123-124 say it only clears features. The engine also reclaims reclaimable units (#7).
- **FEATURES.md:129** says "5-second countdown (cancelled by" Stop. Upstream the countdown is per unit, 2n+1 slow updates (1.5 s for SelfDestructCountdown=1), and Stop doesn't cancel it.
- **FEATURES.md:825-828 cursor table:**
  - Capture and Pickup are listed as 20 frames; they are 14 and 9 (the code has 14 and 9).
  - The Repair row describes a cursor whose click does a move.
- **RM/interaction/selection/groups.rs:100-101** says Spring has no group 0. The engine binds group0 (ENG/Game/UI/KeyBindings.cpp:138).
- **RM/interaction/selection/core.rs:417** credits Q to Spring.
- **RM/interaction/selection/right_click.rs:308-309** says ICON_UNIT_OR_MAP; under newAttackMode it is ICON_UNIT_OR_AREA.
- **Palette comments:** RM/interaction/movement.rs:1057 "palette: move green, fight/attack red, patrol blue, guard white." and RM/interaction/selection/right_click.rs:98 "line color Spring draws for that order kind." claim Spring's palette. The engine has fight 0.5 0.5 1.0 and guard 0.3 0.3 1.0.
- **RM/interaction/movement.rs:146** says AttackMove has "no player keybind". F and the Fight button exist.
- **RM/ui/hud/placement.rs:20** says "the next click only disarms". The code disarms the moment Shift is released.
- **RM/ui/hud/command_panel/commands.rs:7** says Wait is engine-hidden. CMD_WAIT is not hidden.
- **spring-map/src/datavent.rs:3-6** credits the per-vent facing rule to kp_autospam. kp_autospam sets one facing from the homebase.
- **FEATURES.md:712-713** says health bars use faction colours. They use the HP ramp.
- **FEATURES.md:244-245** says minimap dots are coloured by faction. They use team colour.
- **FEATURES.md:439-441** says "There is no progress bar". The build bar has one.

## Checked and matching
- **Right-click orders:** armed units attack an enemy and unarmed ones get nothing; Packets enter an own teleporter; builders Clear a Bad Block feature.
- **Single right-click:** every unit goes to the one point; right-click or Escape disarms an armed command without issuing it.
- **Right-drag formations:** Hungarian matching up to 20 units and NoX beyond, evenly spaced nodes, Shift matching from queue ends, a single unit's path every 50 elmos.
- **Shift** queues every order type.
- **Click selection:** plain replaces, Shift adds, Ctrl toggles. Box-select covers own units including buildings. A click on an enemy clears the selection. A double-click selects the same type in view (the time window differs). Ctrl+A selects all.
- **`,` and `.`** page the panel.
- **Numpad 2/4/6/8** arm the selected builder's minifac; E arms Clear on constructors.
- **Sounds:** the select sound for one unit, MultiSelect for several, `commandgiven` on orders.
- **Bug↔Exploit morph** keeps the selection (RM/units/mechanics/deploy.rs:162).
- **Heroes Right Click** removes itself outside Heroes of Mainframe.
- **Camera:** 45° FOV; camera shake absent in both; no middle-click scroll lock; wheel up zooms in.
- **Selection boxes:** the engine's unit boxes are off (the remake has none).
- **Command queues:** selected units' queues are always drawn, and factory rally queues are drawn from the factory.
- **Geo squares:** trigger, size, colour and blink match. The Bombard and Enter marker colours match the gadgets.
- **Factory clicks:** Shift ×5, Ctrl ×20, both ×100; right-click removes, Alt goes to the front.
- **Command panel:** layout config from KP_CtrlPanel.txt, hidden commands, command and build-option order, the gadgets' command names and tooltips.
- **Placement:** snapping; Shift lines and Alt/Ctrl rectangles; `needShift` for builds.
- **Tips** widget timing and position. BuildETA, team-died messages and the resource bar are absent in both (the whitelist disables them in KP).
- **Command semantics:** the patrol route and Shift-patrol; attack ends when the target dies; area-attack distribution; Stop on a factory clears its rally orders; no self-guard; Bombard range (1150) and Alt; the Enter constants (150, ×0.75, same team, 180-frame stun); AutoHold defaults; pseudo_orders.lua is unreachable (its command descriptions are commented out).

## Unverified leads
- **Shift build boxes:** `buildBoxesOnShift` shows the queued build sites of all own builders while Shift is held. The remake shows only the selected builders' sites; placement.rs not fully read.
- **Space-insert:** Space (fake Meta) + a formation drag inserts at the queue front (KP/LuaUI/Widgets/unit_customformations2.lua:617). The remake has no front-insert; settle by checking for any CMD_INSERT equivalent.
- **NX Flag:** approach and reload (`specialattack.lua` re-issues a ground attack) not compared with RM/units/mechanics/command_fire.rs.
- **Logic bombs:** Launcher.lua's `maxThisUnit` cap not compared.
- **Dispatch on a moving Connection:** without Shift it replaces the Connection's queue upstream, so it stops; the remake's Connection keeps moving.
- **Ctrl + area attack** (removes queued attacks on targets in the circle) is not ported.
- **Tooltip hotkeys:** KP lists every binding ("s Shift+s"); the remake shows one key. The remake's AutoHold hotkey "h" isn't a KP binding (Any+h is sharedialog).
- **Box selection of dying units:** the remake's box query doesn't exclude units playing their death animation (`Dying`), and `Selected` has no insert guard. Settle by checking whether the hover/box queries can hit a `Dying` unit.
