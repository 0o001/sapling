from mercurial.util import httplib
from mercurial import util
import json

urlreq = util.urlreq

# helper class so phabricator_graphql_client can talk using the requests
# third-party library

class PhabricatorClientError(Exception):
    def __init__(self, reason, error):
        Exception.__init__(self, reason, error)

class PhabricatorGraphQLClientRequests(object):

    def __init__(self):
        self._connection = None

    def __verify_connection(self, request_url, timeout, ca_bundle):
        urlparts = urlreq.urlparse(request_url)
        if self._connection is None:
            if urlparts.scheme == 'http':
                self._connection = httplib.HTTPConnection(
                    urlparts.netloc, timeout=timeout)
            elif urlparts.scheme == 'https':
                self._connection = httplib.HTTPSConnection(
                    urlparts.netloc, timeout=timeout, cert_file=ca_bundle)
            else:
                raise PhabricatorClientError('Unknown host scheme: %s',
                                             urlparts.scheme)
        return urlparts

    def sendpost(self, request_url, data, timeout, ca_bundle):
        urlparts = self.__verify_connection(request_url, timeout, ca_bundle)
        query = util.urlreq.urlencode(data)
        headers = {
            'Connection': 'Keep-Alive',
            'Content-Type': 'application/x-www-form-urlencoded',
        }
        self._connection.request('POST', (urlparts.path), query, headers)

        response = json.load(self._connection.getresponse())
        return response
