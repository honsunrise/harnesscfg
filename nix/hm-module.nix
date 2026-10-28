# Home Manager module: declare harness configuration templates; harnesscfg
# seeds and syncs the writable files during activation.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.harnesscfg;
  inherit (lib) mkOption types;

  registry =
    lib.mapAttrs'
      (
        name: _:
        lib.nameValuePair (lib.removeSuffix ".toml" name) (lib.importTOML (../registry + "/${name}"))
      )
      (
        lib.filterAttrs (name: kind: kind == "regular" && lib.hasSuffix ".toml" name) (
          builtins.readDir ../registry
        )
      );

  home = config.home.homeDirectory;
  expand =
    dir:
    let
      xdg = {
        "$XDG_CONFIG_HOME" = config.xdg.configHome;
        "$XDG_DATA_HOME" = config.xdg.dataHome;
        "$XDG_STATE_HOME" = config.xdg.stateHome;
      };
      prefix = lib.findFirst (p: dir == p || lib.hasPrefix "${p}/" dir) null (
        [ "~" ] ++ lib.attrNames xdg
      );
    in
    if prefix == "~" then
      home + lib.removePrefix "~" dir
    else if prefix != null then
      xdg.${prefix} + lib.removePrefix prefix dir
    else
      throw "harnesscfg: unsupported config_dir ${dir}";

  configRoot =
    harness:
    expand (
      if pkgs.stdenv.hostPlatform.isDarwin && harness ? config_dir_macos then
        harness.config_dir_macos
      else
        harness.config_dir
    );

  platform = if pkgs.stdenv.hostPlatform.isDarwin then "macos" else "linux";

  formatOf =
    path:
    let
      ext = lib.last (lib.splitString "." (baseNameOf path));
    in
    {
      json = "json";
      jsonc = "json";
      toml = "toml";
      yaml = "yaml";
      yml = "yaml";
    }
    .${ext} or null;

  syncType = types.submodule {
    freeformType = (pkgs.formats.json { }).type;
    options.kind = mkOption {
      type = types.enum [
        "enabled"
        "preset"
        "providers"
        "patch"
      ];
      description = ''
        Which fields of the file Nix owns; every other field stays with the
        client. Extra attributes are passed through, e.g. `field`,
        `catalogs`, `keys`, `envKeys`, `envPrefixes`, `id`, `file`.
        See docs/MANIFEST.md.
      '';
    };
  };

  fileType = types.submodule (
    { name, config, ... }:
    let
      parts = lib.splitString "/" name;
      harnessId = config.harness;
      harness = registry.${harnessId} or null;
      relative = lib.concatStringsSep "/" (lib.drop 1 parts);
      known = if harness == null then null else lib.findFirst (f: f.name == relative) null harness.file;
    in
    {
      options = {
        enable = mkOption {
          type = types.bool;
          default = true;
          description = "Whether harnesscfg manages this file.";
        };
        harness = mkOption {
          type = types.str;
          default = lib.head parts;
          example = "claude-code";
          description = "Registry id of the harness (see `harnesscfg agents`). Defaults to the first component of the attribute name.";
        };
        source = mkOption {
          type = types.path;
          description = "Template: a file, or a directory bundle. Usually a store path.";
        };
        target = mkOption {
          type = types.str;
          default =
            if harness == null then
              throw "harnesscfg: ${name}: unknown harness ${harnessId}; set target explicitly"
            else
              "${configRoot harness}/${relative}";
          defaultText = lib.literalMD "the harness's configuration directory joined with the rest of the attribute name";
          description = "Writable path the client reads.";
        };
        format = mkOption {
          type = types.nullOr (
            types.enum [
              "json"
              "toml"
              "yaml"
            ]
          );
          default =
            if
              known != null
              && lib.elem known.format [
                "json"
                "toml"
                "yaml"
              ]
            then
              known.format
            else
              formatOf config.target;
          defaultText = lib.literalMD "the registry's format, else the target's extension";
          description = "How to parse the file. `null` for opaque files and directory bundles.";
        };
        seed = mkOption {
          type = types.enum [
            "merge"
            "copy"
          ];
          default = if config.format == null then "copy" else "merge";
          description = ''
            `merge`: when the file is first managed, add missing template keys
            to an existing file. `copy`: only create the file when missing.
          '';
        };
        sync = mkOption {
          type = types.nullOr syncType;
          default = null;
          description = "Nix-owned fields to keep in sync on every activation.";
        };
        alternates = mkOption {
          type = types.listOf types.str;
          default =
            if known != null then
              map (
                ext:
                "${lib.removeSuffix ".${lib.last (lib.splitString "." known.name)}" (baseNameOf known.name)}.${ext}"
              ) (known.alternate_extensions or [ ])
            else
              [ ];
          description = "Sibling file names the client accepts instead; an existing one is used.";
        };
        lock = mkOption {
          type = types.enum [
            "none"
            "omp"
          ];
          default = if harness == null then "none" else harness.lock;
          description = "Cooperative lock the client honours while saving.";
        };
      };
    }
  );

  enabled = lib.filterAttrs (_: f: f.enable) cfg.files;

  manifest = (pkgs.formats.json { }).generate "harnesscfg-manifest.json" {
    version = 2;
    backupDir = cfg.backupDir;
    nixStore = lib.getExe' pkgs.nix "nix-store";
    entries = lib.mapAttrsToList (name: f: {
      inherit name;
      inherit (f)
        harness
        target
        format
        seed
        sync
        alternates
        lock
        ;
      source = "${f.source}";
    }) enabled;
  };

  manifestRelative = lib.removePrefix "${home}/" "${config.xdg.dataHome}/${cfg.manifestPath}";
in
{
  options.programs.harnesscfg = {
    enable = lib.mkEnableOption "harnesscfg, which seeds and syncs AI agent harness configuration";
    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "harnesscfg.packages.\${system}.default";
      description = "The harnesscfg package.";
    };
    files = mkOption {
      type = types.attrsOf fileType;
      default = { };
      example = lib.literalExpression ''
        {
          "codex/config.toml".source = ./codex.toml;
          "omp/models.yml" = {
            source = ./models.yml;
            sync.kind = "providers";
          };
        }
      '';
      description = ''
        Managed files, named `<harness>/<path below its config directory>`.
        The name is the file's identity across generations: renaming it
        makes harnesscfg treat the file as newly managed.
      '';
    };
    manifestPath = mkOption {
      type = types.str;
      default = "harnesscfg/manifest.json";
      description = "Where, below `xdg.dataHome`, each generation records its manifest.";
    };
    backupDir = mkOption {
      type = types.str;
      default = "${config.xdg.stateHome}/harnesscfg";
      defaultText = lib.literalExpression ''"''${config.xdg.stateHome}/harnesscfg"'';
      description = "Originals of overwritten files and GC roots for store paths they reference. May contain credentials.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = lib.flatten (
      lib.mapAttrsToList (name: f: [
        {
          assertion = registry ? ${f.harness} || f.target != null;
          message = "harnesscfg: ${name}: unknown harness ${f.harness}";
        }
        {
          assertion =
            !(registry ? ${f.harness})
            || registry.${f.harness}.platforms or [ ] == [ ]
            || lib.elem platform registry.${f.harness}.platforms;
          message = "harnesscfg: ${name}: ${f.harness} is not documented for ${platform}";
        }
        {
          assertion = f.seed == "copy" || f.format != null;
          message = "harnesscfg: ${name}: seed = \"merge\" needs a format";
        }
        {
          assertion = f.sync == null || f.sync.kind == "patch" || f.format != null;
          message = "harnesscfg: ${name}: sync.kind = \"${f.sync.kind or ""}\" needs a format";
        }
      ]) enabled
    );

    home.packages = [ cfg.package ];
    xdg.dataFile.${cfg.manifestPath}.source = manifest;

    # After the new generation is linked into place is too late: Home Manager
    # would already have removed store links the previous generation owned.
    home.activation.harnesscfg = lib.hm.dag.entryBetween [ "linkGeneration" ] [ "writeBoundary" ] ''
      previous=""
      if [[ -n "''${oldGenPath:-}" ]]; then
        previous="$oldGenPath/home-files/"${lib.escapeShellArg manifestRelative}
      fi
      ${lib.getExe cfg.package} apply ${manifest} --previous "$previous"
    '';
  };
}
