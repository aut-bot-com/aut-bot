import json
import asyncio

from aiohttp import web
import socketio
from aio_pika import IncomingMessage
from jwt.exceptions import InvalidTokenError

from lib.config import which_shard, logger, is_prod, domain_name
from lib.auth import JWT, gateway_authenticated as authenticated
from lib.ipc.async_rpc_client import shardRPC
from lib.ipc.async_subscriber import Subscriber
from lib.ipc.async_rpc_server import start_server
from lib.status_codes import StatusCodes as s
from lib.pool_types import PoolType

from src.pools import GuildPool


sio = socketio.AsyncServer(
    async_mode='aiohttp',
    cors_allowed_origins=[f'https://{domain_name}/', f'https://api.{domain_name}/'] if is_prod else '*'
)
app = web.Application()
sio.attach(app)

loop = asyncio.get_event_loop()
shard_client = shardRPC(loop)
event_subscriber = Subscriber(loop)
manager_client = shardRPC(loop, default_key='manager_rpc')

auth_nonces = {}


class Counter:
    def __init__(self):
        self.count = -2

    def __call__(self):
        self.count += 2
        return self.count


async def register_nonce(method, *args, **kwargs):
    if method == 'register_nonce':
        try:
            auth_nonces[args[0]] = args[1]
        except IndexError:
            pass
        else:
            return {'message': 'registered'}, s.OK_200
    elif method == 'demote_connection':
        # TODO
        return {'message': 'demoted :)'}, s.OK_200
    return {'message': 'invaild arguments'}, s.BAD_REQUEST_400


async def event_callback(msg: IncomingMessage):
    '''handles incoming events from the other services'''
    with msg.process():
        body = json.loads(msg.body.decode())
        guild_id = body['guild_id']
        # private = body['private']
        await sio.emit('log_pool', body, room=f'guild_{guild_id}')
        # print(f"emitting action ({body['action_number']})")

        # sio.emit(body['event'], {'payload': {'message': 'broadcast'}}


class CustomNamespace(socketio.AsyncNamespace):

    def __init__(self, *args, **kwargs):
        self.counter = Counter()

        super().__init__(*args, **kwargs)

    async def on_connect(self, sid: str, environ: dict):
        logger.debug(f"{environ['REMOTE_ADDR']} has connected with sid: {sid}")
        request = environ['aiohttp.request']
        try:
            jwt = JWT(token=request.cookies['token'])
        except (InvalidTokenError, KeyError):
            logger.info("No valid token found, logging into unprivileged gateway...")
        else:
            logger.info("Found valid token, logging into elevated gateway...")
            sio.enter_room(sid, f"{sid}_auth")
            async with sio.session(sid) as session:
                session['jwt'] = jwt

    @authenticated(shard_client)
    async def on_pool_request(self, sid: str, data, jwt):
        _id = data['_id']
        guild_id = data.get('guildId', None)
        type = data['type']
        return_data = []
        nonexistant = []
        for entity in data['ids']:
            resp, sc = await shard_client.pool_request(
                guild_id, type, entity, routing_key=f"shard_rpc_{which_shard(guild_id)}")
            if resp['data']:
                return_data.append(resp['data'])
            else:
                nonexistant.append(entity)

        await sio.emit(
            'pool_response',
            {
                '_id': _id,
                'finished': True,
                'nonexistant': nonexistant,
                'data': return_data,
            },
            room=f"{sid}_auth"
        )

    @authenticated(shard_client)
    async def on_pool_all_request(self, sid: str, data, jwt):
        _id = data['_id']
        guild_id = data.get('guildId', None)
        type = data['type']
        if type == PoolType.GUILD:
            pool = GuildPool(manager_client, shard_client, jwt)

            await sio.emit(
                'pool_response',
                {
                    '_id': _id,
                    'finished': False,
                    'nonexistant': [],
                    'data': await pool.fetch_architus_guilds(),
                },
                room=f"{sid}_auth"
            )
            await sio.emit(
                'pool_response',
                {
                    '_id': _id,
                    'finished': True,
                    'nonexistant': [],
                    'data': await pool.fetch_remaining_guilds(),
                },
                room=f"{sid}_auth"
            )
            return

        else:
            resp, sc = await shard_client.pool_all_request(
                guild_id, type, routing_key=f"shard_rpc_{which_shard(guild_id)}")
            if sc == s.OK_200:
                await sio.emit(
                    'pool_response',
                    {
                        '_id': _id,
                        'finished': True,
                        'nonexistant': [],
                        'data': resp['data'],
                    },
                    room=f"{sid}_auth"
                )
                return
        await sio.emit('error', room=sid)

    def on_disconnect(self, sid: str):
        logger.debug(f'client ({sid}) disconnected')

    async def on_free_elevation(self, sid, data):
        if True and False:
            return
        async with self.session(sid) as session:
            session['jwt'] = JWT(token=data['token'])
            self.enter_room(sid, f"{sid}_auth")

    async def on_request_elevation(self, sid: str, nonce: int):
        logger.debug(f"{sid} requesting elevation...")
        try:
            jwt = JWT(token=auth_nonces[nonce])
            del auth_nonces[nonce]
        except (InvalidTokenError, KeyError):
            logger.info(f"{sid} requested room elevation but didn't provide a valid jwt")
            await sio.emit('elevation_return', {'message': "Missing or invalid jwt"}, room=sid)
        else:
            logger.debug(f"valid nonce provided, granting access...")
            sio.enter_room(sid, f"{sid}_auth")
            await sio.emit('elevation_return', {'message': "success"}, room=sid)
            async with sio.session(sid) as session:
                session['token'] = jwt

    async def on_mock_user_event(self, sid: str, kwargs: dict):
        guild_id = kwargs['guildId']
        resp, _ = await shard_client.handle_mock_user_action(**kwargs, routing_key=f"shard_rpc_{which_shard(guild_id)}")
        await sio.emit('mock_bot_event', resp, room=sid)

    async def on_spectate(self, sid: str, guild_id: int):
        if f"{sid}_auth" not in sio.rooms(sid):
            return
        async with sio.session(sid) as session:
            member, _ = await shard_client.is_member(
                session['token'].id,
                guild_id,
                routing_key=f"shard_rpc_{which_shard(guild_id)}"
            )
            if member:
                sio.enter_room(sid, f"guild_{guild_id}")


async def index(request: web.Request):
    return web.Response(text='this endpoint is for websockets only, get your http out of here >:(',
                        content_type='text/html')
app.router.add_get('/', index)

if __name__ == '__main__':
    async def register_clients(shard_client, event_sub):
        await shard_client.connect()
        await manager_client.connect()
        await (await (await event_sub.connect()).bind_key("gateway.*")).bind_callback(event_callback)

    sio.start_background_task(register_clients, shard_client, event_subscriber)
    sio.start_background_task(start_server, loop, 'gateway_rpc', register_nonce)
    sio.register_namespace(CustomNamespace('/'))
    web.run_app(app, host='0.0.0.0', port='6000')
