import { useTranslation } from "react-i18next";
import { Input } from "@/components/ui/input";
import { SettingsSection } from "@/components/ui/SettingsSection";
import type { SectionProps } from "./types";

function CommandRow({ name, phrase, effect }: { name: string; phrase: string; effect: string }) {
  return (
    <li className="flex gap-2">
      <span className="text-muted-foreground/50">·</span>
      <span className="text-muted-foreground">
        <span className="text-foreground">
          &ldquo;{name}, {phrase}&rdquo;
        </span>{" "}
        — {effect}
      </span>
    </li>
  );
}

export default function AgentSection({ settings, update }: SectionProps) {
  const { t } = useTranslation();
  const aliases = settings.agentAliases;
  const agentDisplayName = settings.agentName || t("agent.name.placeholder");

  return (
    <>
      <SettingsSection
        title={t("agent.name.title")}
        description={t("agent.name.description")}
      >
        <Input
          value={settings.agentName}
          onChange={(e) => update("agentName", e.target.value)}
          placeholder={t("agent.name.placeholder")}
          className="w-48 h-9 text-sm"
        />
      </SettingsSection>

      <SettingsSection
        title={t("agent.commands.title")}
        description={t("agent.commands.description")}
      >
        {/* Written out rather than mapped over a key list: the i18n keys are
            strongly typed, so a template-built key doesn't narrow. */}
        <ul className="space-y-1.5 text-sm">
          <CommandRow name={agentDisplayName} phrase={t("agent.commands.notes.phrase")} effect={t("agent.commands.notes.effect")} />
          <CommandRow name={agentDisplayName} phrase={t("agent.commands.conversation.phrase")} effect={t("agent.commands.conversation.effect")} />
          <CommandRow name={agentDisplayName} phrase={t("agent.commands.persona.phrase")} effect={t("agent.commands.persona.effect")} />
          <CommandRow name={agentDisplayName} phrase={t("agent.commands.stop.phrase")} effect={t("agent.commands.stop.effect")} />
        </ul>
      </SettingsSection>

      <SettingsSection
        title={t("agent.aliases.title")}
        description={t("agent.aliases.description")}
      >
        <div className="space-y-2">
          {[0, 1].map((i) => (
            <Input
              key={i}
              value={aliases[i] ?? ""}
              onChange={(e) => {
                const updated = [aliases[0] ?? "", aliases[1] ?? ""];
                updated[i] = e.target.value;
                update("agentAliases", updated.filter((a) => a.trim() !== ""));
              }}
              placeholder={i === 0 ? t("agent.aliases.placeholder1") : t("agent.aliases.placeholder2")}
              className="w-48 h-9 text-sm"
            />
          ))}
        </div>
      </SettingsSection>
    </>
  );
}
