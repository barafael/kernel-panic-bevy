# Kernel Panic: the RTS waged inside your computer

**Kernel Panic is a free, open-source real-time strategy game built on the Spring engine where three factions — System, Hacker, and Network — battle inside a computer, with units representing processes, malware, and data packets.** The game's defining innovation is the complete elimination of resource management: every unit and building is free to construct, making time and map control the only constraints. Originally created by KDR_11k with concept by Boirunner, and maintained since the mid-2000s by zwzsg, Kernel Panic reached version 4.9 in June 2021 and remains playable today, though its community has largely gone dormant. Among Spring engine games, it stands apart as one of the few with zero connection to Total Annihilation — a fully original design that trades the genre's typical hour-long economic grinds for intense **5-to-10-minute matches** with a learning curve measured in minutes.

## A computer-themed RTS that strips the genre to its core

Kernel Panic takes the real-time strategy formula and removes nearly everything except combat and positioning. There is no metal, no energy, no harvesting, and no supply chain. Every factory, once placed on a pre-defined "datavent" location, automatically and continuously produces units at no cost. The only constraints are build time and physical space on the map. This radical simplification — frequently compared to Bitmap Brothers' classic **Z** (1996) — transforms the RTS experience into pure tactical action. Typical matches resolve in under ten minutes.

The visual style reinforces the "inside a computer" conceit with a **vectorial, wireframe aesthetic** reminiscent of Tron and Darwinia. Maps resemble circuit boards and memory arrays, rendered in glowing lines against dark backgrounds with fluorescent color palettes. The tech tree is deliberately minimal at roughly ten units per faction (including factories and defensive structures), meaning a new player can learn every unit's purpose within a single match. The game ships with no campaign — only skirmish and multiplayer modes — but includes three AI opponents of varying difficulty and several game mode variants.

### Game modes beyond the standard

While the default mode tasks players with destroying all enemy homebases and factories, several alternative modes ship with the game:

- **Color Wars** — a territory-control mode where map dominance determines the winner when time expires
- **Heroes of Mainframe (HOMF)** — each player controls a single heavily buffed unit, turning the game into something closer to an action-RPG
- **O.N.S. (Onslaught)** — adds shields to buildings to prevent early homebase rushes
- **Save Our Mem** — dying units leave "memory leaks" that must be reclaimed
- **Pre-placed Minifacs** — factories start already built on the map, eliminating the expansion phase entirely

## Three factions built from computing metaphors

Each faction maps its entire unit roster onto a domain of computer science, creating a thematically coherent roster where every name carries meaning.

### System: the beginner-friendly OS faction

System represents the operating system itself and is recommended for new players as the most conventional faction. Its **Kernel** serves as the main base and primary factory, producing units directly. **Bits** — named for the smallest unit of data — are the basic swarm unit, cheap and fast to mass-produce. Sockets (secondary factories built on datavents by the mobile **Assembler** constructor) auto-produce Bits continuously once constructed.

The faction's backbone consists of two specialized combat units. The **Byte** is a defensive unit with approximately 15,000 HP that takes only 30% damage while in its "closed" state, though it has a critical blind spot directly beneath it. The **Pointer** is the faction's game-winning offensive unit — a deployable artillery piece with devastating range that can shell enemy bases from elevated positions. The Pointer's special **NX Flag** ability (activated via the d-gun hotkey) fires a single shot that sets an area ablaze for a full minute. Pointers are vulnerable to stealthy units and close-range swarms but dominate when given line of sight from high ground, particularly on the iconic Marble Madness map where controlling the central hill often decides the match.

### Hacker: stealth, infection, and disruption

The Hacker faction draws from the vocabulary of computer security threats and plays with an emphasis on stealth and unit conversion. Its homebase, the **Hole** (as in "security hole"), functions similarly to the Kernel. **Bugs** serve as the basic swarm unit and can morph into stationary **Exploits** — artillery emplacements that deal increasing damage at greater range.

The Hacker's signature unit is the **Worm**, a cloaked ambusher that moves at full speed while invisible. When a Worm kills enemy units, those units are **converted into Viruses** fighting for the Hacker — a mechanic that can snowball devastatingly if the Worm catches a dense swarm. The **DOS** unit (Denial of Service) stuns and paralyzes enemy units, disabling key defenses. **Viruses** themselves cannot be directly produced; they only spawn from worm kills, creating a self-propagating swarm mechanic true to their namesake. **Windows** serve as the faction's secondary factories built on datavents. The Hacker rewards patient, tactical play — positioning cloaked Worms for ambushes and using DOS to neutralize high-value targets before the Worm strikes.

### Network: teleportation and instant reinforcement

The Network faction is built around the most mechanically distinctive system in the game: the **Buffer**. Unlike other factions, Network's factories (**Ports**) do not visibly produce units. Instead, they increment a virtual Buffer counter. The player can then **materialize Packets** (the faction's main combat unit) instantly at any Port or **Connection** (the faction's homebase, which doubles as a teleporter) anywhere on the map. Packets can also be dematerialized back into the Buffer by entering a teleporter, enabling rapid redeployment across the entire battlefield.

This mechanic means **every Network factory is inherently defended** — on attacking a Port, a network player can instantly spawn Packets from the Buffer. The faction excels at mobility and map presence, capable of reinforcing any position without the travel time that constrains other factions. Opponents must account for the fact that an apparently undefended Port can materialize a full army in seconds.

## Development history spanning nearly two decades

Kernel Panic's origins trace to the mid-2000s within the Spring RTS community. **Boirunner** conceived the original concept, while **KDR_11k** did the bulk of development work. **Noruas** created the sound design. In November 2007, KDR_11k released **Division Zero**, a fork of version 1.5 that added several new units including the Terminal, Obelisk, and refined the Exploit and Virus mechanics.

From approximately version 3.x onward, maintenance passed to **zwzsg**, a prolific Spring community member who shepherded the game through over a decade of engine compatibility updates. Each new Spring engine release tended to break something — from Lua API changes to pathfinding regressions to renamed armor classes — and zwzsg patiently patched each break.

| Version | Date | Spring Engine | Notable changes |
|---------|------|---------------|-----------------|
| 1.x–2.x | ~2006–2007 | Early Spring | Original development by KDR_11k |
| Division Zero | Nov 2007 | >75b2 | KDR_11k's fork adding Virus, Terminal, Obelisk units |
| 3.2 | ~2008–2009 | 0.78.2.1 | First widely distributed installer by zwzsg (~25 MB) |
| 4.1 | ~2010–2011 | 0.82.5.1 | HeatMapping fix, game_spawn.lua rewrite; packaged for Ubuntu |
| 4.2 | May 2011 | 0.82.7.1 | — |
| 4.4 | Jan 2012 | 85.0 | — |
| 4.6 | ~2014 | 95.0 | Custom loadscreen, major Spring 95 compatibility fixes |
| 4.7 | Jun 2017 | 103.0 | Cross-platform wxLua launcher, QTPFS pathfinding |
| 4.8 | Apr 2019 | — | Incremental fixes |
| **4.9** | **Jun 25, 2021** | **105.0** | Latest release; fixes for Spring 105 API changes |

The game was also ported to the **Recoil** engine fork (a successor/fork of Spring) by GitHub user **sprunk**, who imported version 4.9 and updated it for Recoil compatibility. This repository at **github.com/sprunk/Kernel-Panic** represents the only Git-hosted version of the game, as zwzsg expressed philosophical reluctance about GitHub's centralization: "It annoys me how compulsory Github has become. I don't like that kind of centralised monoculture."

## Where to get it and how to run it

The game remains fully downloadable and playable. The primary distribution channel is **ModDB** (moddb.com/games/kernel-panic), where the version 4.9 all-in-one Windows installer weighs **134.6 MB** and bundles Spring engine 105.0, the game mod, maps, and a launcher. A backup archive exists on the Internet Archive mirroring zwzsg's original hosting. Linux users can install via the **`spring-mods-kernelpanic`** Ubuntu/Debian package (though this packages the older version 4.1) or download the `.sd7` mod file directly and pair it with a Spring engine installation. Arch Linux offers a **`spring-kp`** package.

For existing Spring engine users, the game is simply a `.sd7` archive placed in the engine's mods directory, with map files in the maps directory. Multiplayer can be accessed through SpringLobby (with port forwarding on port 8452), the Zero-K lobby infrastructure, or LAN play via Uberserver. There is no dedicated SourceForge project for Kernel Panic — the `kernelpanic.sourceforge.net` URL hosts an entirely unrelated project.

**System requirements** are minimal, inheriting only the Spring engine's baseline needs: any modern Windows or Linux system with basic 3D graphics support will run the game comfortably, given its deliberately simple vectorial art style.

## Reception, community, and the long tail of a niche classic

Kernel Panic earned a **9.8/10 average rating** from 23 votes on ModDB, accumulated roughly **2,300 downloads** across all installer versions there, and gathered 73 followers — modest numbers reflecting its niche audience rather than its quality. The game was featured as **"Game of the Day"** on Finnish tech site mbnet.fi in May 2011. LinuxLinks recommended it as a standout free Linux game, praising its "fast-paced, intense action" and short learning curve while noting it "can be challenging to master." Wikipedia's Spring Engine article describes it as "a Darwinia-esque game emphasizing simplicity."

Community reception on the Penny Arcade forums captured the game's dual nature well: users praised the distinctive art style and accessibility but noted the gameplay was "not very deep." The developer's own counter-argument was characteristically blunt: "I believe it's the best [RTS]. Other RTS might sound good on paper, but KP has better ratio of fun / boringness." The SpringRTS forum hosted a dedicated **88-topic subforum** for Kernel Panic, active primarily from 2008 through 2021.

Within the Spring engine ecosystem, Kernel Panic occupies a unique position. While most Spring games (Balanced Annihilation, XTA, NOTA) descend from Total Annihilation's content and mechanics — featuring deep economies, hundreds of units, and matches lasting an hour or more — Kernel Panic shares none of that DNA. It is **entirely original content** with zero TA heritage, categorized alongside Zero-K, Spring: 1944, and Evolution RTS as an "Open Source" game on the Spring wiki. Where Zero-K offers hundreds of units and complex metal economies, Kernel Panic offers ten units and zero economy. The two games represent opposite philosophies of what an RTS can be, both built on the same engine.

No formal tournaments were ever organized, and the multiplayer community has been effectively dormant for years. The game's primary single-player value comes from its three AI opponents: the adaptive **Fair KPAI** (which scales to the player's skill), the unrestricted **Kernel Panic AI** (which knows every unit ability), and **Baczek's KP AI** (a C++ implementation by imbaczek with more sophisticated attack grouping). Watching AI-versus-AI battles serves as both entertainment and an effective tutorial.

## Conclusion: a design statement preserved in code

Kernel Panic's lasting significance lies not in its player count but in its design thesis — that an RTS can be compelling with zero economic management, ten units, and five-minute matches. It proved this thesis convincingly enough to sustain over fifteen years of maintenance through relentless engine-breaking updates, earn near-perfect user ratings, and inspire a Recoil engine port years after active development ceased. The game's licensing (mostly Public Domain with some GPL Lua scripts and CC-BY-SA maps) ensures it will remain freely available indefinitely. For anyone curious about what real-time strategy looks like stripped to pure tactical essence, wrapped in a Tron-inspired computer metaphor, Kernel Panic remains immediately playable — download the 134 MB installer from ModDB, and you'll understand the entire game within your first match.
