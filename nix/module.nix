# NixOS module: `programs.days-of-our-lives`. Writes the global configuration to
# /etc/days-of-our-lives/ (users override it in ~/.config/days-of-our-lives/).
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.days-of-our-lives;
  toml = pkgs.formats.toml { };
  # Plugins are found under systemd's minimal PATH too.
  settings = { plugin_path = [ "${cfg.package}/bin" ]; } // cfg.settings;
in
{
  options.programs.days-of-our-lives = {
    enable = lib.mkEnableOption "the days-of-our-lives desktop bar";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "Bar plus plugins (all dool-plugin-* executables in its bin/).";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = { bars.main = { output = "DP-1"; edge = "top"; }; };
      description = "Contents of /etc/days-of-our-lives/config.toml.";
    };

    style = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = "Contents of /etc/days-of-our-lives/style.css (GTK4 CSS).";
    };

    plugins = lib.mkOption {
      type = lib.types.attrsOf toml.type;
      default = { };
      example = { clock = { plugin = "clock"; position = { bar = "main"; section = "start"; }; }; };
      description = "Plugin instances: one /etc/days-of-our-lives/plugins.d/<name>.toml per attribute.";
    };

    systemd.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Start the bar as a systemd user service with the graphical session.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    environment.etc =
      {
        "days-of-our-lives/config.toml".source = toml.generate "config.toml" settings;
      }
      // lib.optionalAttrs (cfg.style != "") {
        "days-of-our-lives/style.css".text = cfg.style;
      }
      // lib.mapAttrs' (name: value: lib.nameValuePair "days-of-our-lives/plugins.d/${name}.toml" {
        source = toml.generate "${name}.toml" value;
      }) cfg.plugins;

    systemd.packages = lib.mkIf cfg.systemd.enable [ cfg.package ]; # installs share/systemd/user units
    systemd.user.services.days-of-our-lives.wantedBy = lib.mkIf cfg.systemd.enable [ "graphical-session.target" ];
  };
}
