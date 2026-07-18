import "npm:reflect-metadata@0.2.2";
import "npm:class-transformer@0.5.1";

import {
  Body,
  CallHandler,
  CanActivate,
  Controller,
  ExecutionContext,
  Get,
  Injectable,
  Module,
  NestInterceptor,
  Param,
  Post,
  StreamableFile,
  UseGuards,
  UseInterceptors,
  ValidationPipe,
} from "npm:@nestjs/common@11.1.6";
import { NestFactory } from "npm:@nestjs/core@11.1.6";
import {
  FastifyAdapter,
  NestFastifyApplication,
} from "npm:@nestjs/platform-fastify@11.1.6";
import { IsString, MinLength } from "npm:class-validator@0.14.2";
import { Readable } from "node:stream";

class CreateMessageDto {
  @IsString()
  @MinLength(3)
  message!: string;
}

@Injectable()
class CounterService {
  private value = 0;

  next(): number {
    this.value += 1;
    return this.value;
  }
}

@Injectable()
class HeaderGuard implements CanActivate {
  canActivate(context: ExecutionContext): boolean {
    const request = context.switchToHttp().getRequest();
    return request.headers["x-test-auth"] === "allowed";
  }
}

@Injectable()
class MarkerInterceptor implements NestInterceptor {
  intercept(context: ExecutionContext, next: CallHandler) {
    context.switchToHttp().getResponse().header("x-nest-interceptor", "active");
    return next.handle();
  }
}

@Controller()
@UseInterceptors(MarkerInterceptor)
class AppController {
  constructor(private readonly counter: CounterService) {}

  @Get()
  root() {
    return {
      framework: "nestjs",
      adapter: "fastify",
      count: this.counter.next(),
    };
  }

  @Get("users/:id")
  user(@Param("id") id: string) {
    return { user: id };
  }

  @UseGuards(HeaderGuard)
  @Get("guarded")
  guarded() {
    return { guarded: true };
  }

  @Post("validate")
  validate(@Body() body: CreateMessageDto) {
    return { message: body.message };
  }

  @Get("stream")
  stream() {
    return new StreamableFile(Readable.from(["nest-", "fastify-stream"]));
  }
}

@Module({
  controllers: [AppController],
  providers: [CounterService, HeaderGuard, MarkerInterceptor],
})
class AppModule {}

const app = await NestFactory.create<NestFastifyApplication>(
  AppModule,
  new FastifyAdapter(),
  { logger: ["error", "warn"] },
);
app.useGlobalPipes(new ValidationPipe({ transform: true }));
await app.listen(
  Number(Deno.env.get("FRAMEWORK_TEST_PORT") ?? "3000"),
  "127.0.0.1",
);
