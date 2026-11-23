# Learning plan format

socratui can read a learning plan: a JSON file that lists skills to learn, each with a weight. Any tool can write one. socratui ranks the skills and shows them on the plan screen.

## Turning the plan on

The plan is off by default. While it is off, socratui never reads the plan file. To turn it on, press `s` for settings, move to *Learning plan* and choose *On*. Then `p` on the home screen opens the plan.

## Where the file lives

socratui reads `plan.json` in its data directory. To use another file, set *Plan file* in settings. An empty value goes back to `plan.json`. A leading `~` stands for your home directory.

Settings saves both values in `config.json`, in the same directory:

```json
{ "plan_enabled": true, "plan_path": "~/plans/socratui.json" }
```

## Example

```json
{
  "format": "socratui-plan/1",
  "generated_at": "2026-11-23T15:00:00Z",
  "source": "example",
  "summary": "412 sources, last 180 days",
  "skills": [
    {
      "id": "kubernetes",
      "name": "Kubernetes",
      "weight": 0.92,
      "start_level": 1,
      "facts": ["38 mentions", "rising"],
      "trend": "up",
      "prerequisites": ["docker"],
      "related": ["terraform", "helm"],
      "former_ids": ["k8s"]
    }
  ],
  "tracks": [
    {
      "id": "ml-platform-engineer",
      "name": "ML platform engineer",
      "facts": ["70% overlap"],
      "why": "Your Python and Kubernetes plus two MLOps tools",
      "skills": ["ray", "mlflow"]
    }
  ]
}
```

## Fields

Top level:

- `format` (required): must be `socratui-plan/1`.
- `skills` (required): the list of skills.
- `generated_at`: when the file was made, as an ISO 8601 time.
- `source`: the name of the tool that made the file.
- `summary`: one line about the data behind the plan.
- `tracks`: optional groups of skills that go together.

Each skill:

- `id` (required): a stable key, such as `kubernetes`.
- `name` (required): the name to show.
- `weight` (required): how much the skill matters, from 0 to 1. Values outside that range are cut to 0 or 1.
- `start_level`: the level, 0 to 5, to start from if you have no history for the skill. Values outside that range are cut to 0 or 5.
  If the learner already has a topic with the same name made by hand, starting the skill from the plan creates a separate topic.
- `facts`: short notes to show beside the skill.
- `trend`: `up`, `down` or `flat`.
- `prerequisites`: ids of skills to learn first.
- `related`: ids of similar skills.
- `former_ids`: earlier ids of this skill, so that a rename keeps your progress.

Each track:

- `id` and `name` (required).
- `facts`: short notes to show beside the track.
- `why`: one sentence on why the track fits you.
- `skills`: ids of the skills in the track.

Fields that socratui does not know are ignored.

## Versions

The number after `socratui-plan/` is the version. This socratui reads version 1 only. A file with another version is refused with a message that names it.
