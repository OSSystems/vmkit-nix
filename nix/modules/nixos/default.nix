# What every controller of cloud-hypervisor VMs on one host shares: guest
# networks isolated from the host and private ranges and NATed out, and the
# memory pools that account for each other. Controllers declare their
# networks and pools here; this module renders them once.
{
  config,
  lib,
  ...
}:

let
  inherit (lib) mkOption types;
  cfg = config.services.vmkit;

  privateRanges = [
    "10.0.0.0/8"
    "100.64.0.0/10"
    "169.254.0.0/16"
    "172.16.0.0/12"
    "192.168.0.0/16"
  ];

  # Guests reach nothing on the host but replies, and from the forward
  # rules on only what is not private.
  start =
    chain: net:
    ''
      iptables -N ${chain}-in 2>/dev/null || iptables -F ${chain}-in
      iptables -A ${chain}-in -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
      iptables -A ${chain}-in -j DROP
      iptables -D INPUT -i ${net.interface} -j ${chain}-in 2>/dev/null || true
      iptables -I INPUT -i ${net.interface} -j ${chain}-in

      iptables -N ${chain}-fwd 2>/dev/null || iptables -F ${chain}-fwd
    ''
    + net.forwardRules
    + ''
      for net in ${lib.concatStringsSep " " privateRanges}; do
        iptables -A ${chain}-fwd -d "$net" -j DROP
      done
      iptables -D FORWARD -i ${net.interface} -j ${chain}-fwd 2>/dev/null || true
      iptables -I FORWARD -i ${net.interface} -j ${chain}-fwd
    ''
    + lib.optionalString (net.blockIPv6 && config.networking.enableIPv6) ''
      ip6tables -D INPUT -i ${net.interface} -j DROP 2>/dev/null || true
      ip6tables -I INPUT -i ${net.interface} -j DROP
      ip6tables -D FORWARD -i ${net.interface} -j DROP 2>/dev/null || true
      ip6tables -I FORWARD -i ${net.interface} -j DROP
    '';

  stop =
    chain: net:
    ''
      iptables -D INPUT -i ${net.interface} -j ${chain}-in 2>/dev/null || true
      iptables -D FORWARD -i ${net.interface} -j ${chain}-fwd 2>/dev/null || true
    ''
    + lib.optionalString (net.blockIPv6 && config.networking.enableIPv6) ''
      ip6tables -D INPUT -i ${net.interface} -j DROP 2>/dev/null || true
      ip6tables -D FORWARD -i ${net.interface} -j DROP 2>/dev/null || true
    '';

  networkModule =
    { name, ... }:
    {
      options = {
        subnet = mkOption {
          type = types.strMatching "[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}";
          example = "10.231.0";
          description = "First three octets of the network's /24; the host is .1.";
        };
        interface = mkOption {
          type = types.str;
          example = "vmbr0";
          description = "Interface guests come in on, as iptables matches it (`tap+` matches every tap).";
        };
        chain = mkOption {
          type = types.strMatching "[A-Za-z0-9_-]{1,24}";
          default = name;
          description = "Prefix of the network's iptables chains, `<chain>-in` and `<chain>-fwd`.";
        };
        forwardRules = mkOption {
          type = types.lines;
          default = "";
          description = ''
            iptables commands run after `<chain>-fwd` is flushed and before
            private ranges are dropped: `-j RETURN` allows a destination,
            `-j DROP` refuses one.
          '';
        };
        blockIPv6 = mkOption {
          type = types.bool;
          default = false;
          description = "Drop every IPv6 packet guests send.";
        };
      };
    };

  poolModule =
    { name, ... }:
    {
      options = {
        rank = mkOption {
          type = types.enum [
            "first"
            "yielding"
          ];
          description = ''
            First pools grow before yielding ones: a yielding pool past its
            guarantee counts what first pools hold as taken, and stops
            growing and booting while a first pool was denied memory.
          '';
        };
        guaranteedGiB = mkOption {
          type = types.ints.unsigned;
          default = 0;
          description = "Yielding pools only: memory the pool may hold whatever first pools want.";
        };
        statusFile = mkOption {
          type = types.str;
          readOnly = true;
          default = "${cfg.memory.statusDir}/${name}.json";
          description = "Where the pool's controller publishes the memory it holds.";
        };
      };
    };
in
{
  key = "github:OSSystems/vmkit-nix#nixosModules.default";

  options.services.vmkit = {
    externalInterface = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Interface guest networks are NATed out of.";
    };

    networks = mkOption {
      type = types.attrsOf (types.submodule networkModule);
      default = { };
      description = "Guest networks: isolated from the host and private ranges, NATed out of externalInterface.";
    };

    memory = {
      reservePercent = mkOption {
        type = types.ints.between 0 99;
        default = 10;
        description = "Share of host memory, at least reserveMinGiB, no pool boots or grows a VM into.";
      };
      reserveMinGiB = mkOption {
        type = types.ints.unsigned;
        default = 2;
        description = "Least memory kept for the host.";
      };
      statusDir = mkOption {
        type = types.str;
        readOnly = true;
        default = "/run/vmkit/memory";
        description = "Where pools publish what they hold.";
      };
      pools = mkOption {
        type = types.attrsOf (types.submodule poolModule);
        default = { };
        description = "Pools of VMs whose controllers share host memory, keyed by name.";
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf (cfg.networks != { }) {
      assertions =
        let
          subnets = lib.mapAttrsToList (_: n: n.subnet) cfg.networks;
          chains = lib.mapAttrsToList (_: n: n.chain) cfg.networks;
        in
        [
          {
            assertion = lib.length subnets == lib.length (lib.unique subnets);
            message = "services.vmkit.networks: every network needs its own subnet.";
          }
          {
            assertion = lib.length chains == lib.length (lib.unique chains);
            message = "services.vmkit.networks: every network needs its own chain.";
          }
          {
            assertion = cfg.externalInterface != null;
            message = "services.vmkit.externalInterface must be set for guest networks to reach out.";
          }
        ];

      networking = {
        nat = {
          enable = true;
          internalInterfaces = lib.mapAttrsToList (_: n: n.interface) cfg.networks;
          inherit (cfg) externalInterface;
        };
        firewall = {
          extraCommands = lib.concatStrings (lib.mapAttrsToList (_: n: start n.chain n) cfg.networks);
          extraStopCommands = lib.concatStrings (lib.mapAttrsToList (_: n: stop n.chain n) cfg.networks);
        };
      };
    })

    (lib.mkIf (cfg.memory.pools != { }) {
      systemd.tmpfiles.rules = [ "d ${cfg.memory.statusDir} 0755 root root -" ];
    })
  ];
}
