# Brave debloat

Run `wsctl brave debloat ./brave.mobileconfig` to generate an Apple configuration profile. Open the file and install it in System Settings > General > Device Management. Restart Brave, then check `brave://policy` for the applied values.

The profile disables Rewards, Wallet, VPN, AI Chat, local AI, News, Talk, Playlist, Web Discovery, Speedreader, Wayback Machine, Email Aliases, and Tor. It disables P3A, the stats ping, and crash reporting. It pins extended Safe Browsing reporting and URL-keyed data collection off; Brave already defaults to those settings. It leaves Shields and Safe Browsing protection settings alone.

These policies control the existing Brave build. They do not remove compiled features or guarantee that the browser makes no network requests. macOS shows Brave as managed while the profile is installed. Remove the profile in Device Management to restore unmanaged settings.
