import requests
try:
    import aiohttp
except ImportError:
    pass

from lib.config import client_id, client_secret, REDIRECT_URI, API_ENDPOINT, secret_token

template = {
    'client_id': client_id,
    'client_secret': client_secret,
    'redirect_uri': REDIRECT_URI,
    'scope': 'identify',
}

headers = {'Content-Type': 'application/x-www-form-urlencoded'}


def token_exchange_request(code):
    data = template.copy()
    data['code'] = code
    data['grant_type'] = 'authorization_code',

    r = requests.post('%s/oauth2/token' % API_ENDPOINT, data=data, headers=headers)

    return r.json(), r.status_code


def identify_request(token):
    headers = {
        'Content-Type': 'application/x-www-form-urlencoded',
        'Authorization': f"Bearer {token}"
    }
    r = requests.get('%s/users/@me' % API_ENDPOINT, headers=headers)

    return r.json(), r.status_code


def list_guilds_request(jwt):
    headers = {
        'Content-Type': 'application/x-www-form-urlencoded',
        'Authorization': f"Bearer {jwt.access_token}"
    }
    r = requests.get('%s/users/@me/guilds' % API_ENDPOINT, headers=headers)

    return r.json(), r.status_code


async def async_list_guilds_request(jwt):
    headers = {
        'Content-Type': 'application/x-www-form-urlencoded',
        'Authorization': f"Bearer {jwt.access_token}"
    }
    async with aiohttp.ClientSession() as session:
        url = f"{API_ENDPOINT}/users/@me/guilds"
        async with session.get(url, headers=headers) as resp:
            return await resp.json(), resp.status


def refresh_token_request(refresh_token):
    data = template.copy()
    data['grant_type'] = 'refresh_token',
    data['refresh_token'] = refresh_token
    r = requests.post(f'{API_ENDPOINT}/oauth2/token', data=data, headers=headers)
    return r.json(), r.status_code


def register_command(json_cmd):
    url = f"{API_ENDPOINT}/applications/{client_id}/commands"
    headers = {
        "Authorization": f"Bot {secret_token}"
    }
    r = requests.post(url, headers=headers, json=json_cmd)
    return r.json(), r.status_code


def register_guild_command(guild_id, json_cmd):
    url = f"{API_ENDPOINT}/applications/{client_id}/guilds/{guild_id}/commands"
    headers = {
        "Authorization": f"Bot {secret_token}"
    }
    r = requests.post(url, headers=headers, json=json_cmd)
    return r.json(), r.status_code
