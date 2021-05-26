from lib.discord_requests import register_command, register_guild_command
from lib.auth import verify_discord_interaction
from src.util import CustomResource



commands = []
with open('./src/slash_commands/set.json') as f:
    commands.append(f.read())

def init():
    for c in commands:
        register_command(c)
        register_command(436189230390050826, c)

class DiscordInteraction(CustomResource):
    @verify_discord_interaction
    def post(self):
        return {
            "type": 4,
            "data": {
                "tts": False,
                "content": "Congrats on sending your command!",
                "embeds": [],
                "allowed_mentions": { "parse": [] }
            }
        }