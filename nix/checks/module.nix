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
              batch = {
                subnet = "10.231.0";
                interface = "vmbr0";
              };
              interactive = {
                subnet = "10.232.0";
                interface = "vm+";
                blockIPv6 = true;
                forwardRules = "iptables -A interactive-fwd -d 10.0.5.20 -j RETURN\n";
              };
            };
            memory.pools = {
              interactive.rank = "first";
              batch = {
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
  sameSubnet = eval { services.vmkit.networks.interactive.subnet = lib.mkForce "10.231.0"; };
  fw = config.networking.firewall.extraCommands;
  allow = lib.strings.splitString "\n" fw;
  index = needle: lib.lists.findFirstIndex (lib.hasPrefix needle) null allow;
in
assert failed config == [ ];
assert failed sameSubnet == [ "services.vmkit.networks: every network needs its own subnet." ];
assert
  config.networking.nat.internalInterfaces == [
    "vmbr0"
    "vm+"
  ];
assert lib.hasInfix "iptables -I INPUT -i vmbr0 -j batch-in" fw;
assert lib.hasInfix "ip6tables -I FORWARD -i vm+ -j DROP" fw;
assert !(lib.hasInfix "ip6tables -I FORWARD -i vmbr0" fw);
# An allowed endpoint comes before the private ranges are dropped.
assert
  index "iptables -A interactive-fwd -d 10.0.5.20 -j RETURN"
  < index "  iptables -A interactive-fwd -d \"$net\" -j DROP";
assert config.services.vmkit.memory.pools.batch.statusFile == "/run/vmkit/memory/batch.json";
assert lib.elem "d /run/vmkit/memory 0755 root root -" config.systemd.tmpfiles.rules;
pkgs.runCommand "vmkit-module" { } "touch $out"
