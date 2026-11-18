{
  pkgs,
  flake,
  inputs,
  ...
}:
let
  inherit (pkgs) lib;
  eval =
    extra:
    (inputs.nixpkgs.lib.nixosSystem {
      inherit (pkgs.stdenv.hostPlatform) system;
      modules = [
        # Two controllers each import vmkit; it is declared once.
        flake.nixosModules.default
        flake.nixosModules.default
        {
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          system.stateVersion = "26.05";
          services.vmkit = {
            externalInterface = "eth0";
            networks = {
              runners = {
                subnet = "10.231.0";
                interface = "ghr0";
              };
              workspaces = {
                subnet = "10.232.0";
                interface = "ws+";
                blockIPv6 = true;
                forwardRules = "iptables -A workspaces-fwd -d 10.0.5.20 -j RETURN\n";
              };
            };
            memory.pools = {
              workspaces.rank = "first";
              runners = {
                rank = "yielding";
                guaranteedGiB = 8;
              };
            };
          };
        }
        extra
      ];
    }).config;
  config = eval { };
  failed = c: map (a: a.message) (lib.filter (a: !a.assertion) c.assertions);
  sameSubnet = eval { services.vmkit.networks.workspaces.subnet = lib.mkForce "10.231.0"; };
  fw = config.networking.firewall.extraCommands;
  allow = lib.strings.splitString "\n" fw;
  index = needle: lib.lists.findFirstIndex (lib.hasPrefix needle) null allow;
in
assert failed config == [ ];
assert failed sameSubnet == [ "services.vmkit.networks: every network needs its own subnet." ];
assert
  config.networking.nat.internalInterfaces == [
    "ghr0"
    "ws+"
  ];
assert lib.hasInfix "iptables -I INPUT -i ghr0 -j runners-in" fw;
assert lib.hasInfix "ip6tables -I FORWARD -i ws+ -j DROP" fw;
assert !(lib.hasInfix "ip6tables -I FORWARD -i ghr0" fw);
# An allowed endpoint comes before the private ranges are dropped.
assert
  index "iptables -A workspaces-fwd -d 10.0.5.20 -j RETURN"
  < index "  iptables -A workspaces-fwd -d \"$net\" -j DROP";
assert config.services.vmkit.memory.pools.runners.statusFile == "/run/vmkit/memory/runners.json";
assert lib.elem "d /run/vmkit/memory 0755 root root -" config.systemd.tmpfiles.rules;
pkgs.runCommand "vmkit-module" { } "touch $out"
